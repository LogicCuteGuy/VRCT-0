use crate::{
    evaluation::load_runtime,
    graph::{atomic_write, emit, float_tensor, int, value_info},
    training, Result,
};
use onnx_protobuf::{GraphProto, Message, ModelProto, TensorProto};
use std::{collections::BTreeMap, path::Path};

pub fn floats(t: &TensorProto) -> Result<Vec<f32>> {
    if t.data_type != 1 || t.dims.iter().any(|&d| d < 0) {
        return Err(format!("expected FLOAT initializer {}", t.name).into());
    }
    let expected = t.dims.iter().try_fold(1usize, |n, &d| {
        n.checked_mul(d as usize).ok_or("tensor size overflow")
    })?;
    let data = if !t.raw_data.is_empty() {
        if !t.raw_data.len().is_multiple_of(4) {
            return Err("malformed FLOAT tensor bytes".into());
        }
        t.raw_data
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
            .collect()
    } else {
        t.float_data.clone()
    };
    if data.len() != expected || data.iter().any(|v| !v.is_finite()) {
        return Err(format!("invalid initializer {}", t.name).into());
    }
    Ok(data)
}
pub fn excluded(name: &str) -> bool {
    name.contains("_preds.")
        || name.contains("_preds/")
        || name.contains("Decode")
        || name == "/head/Outputs"
        || name.starts_with("/head/Concat")
        || name.starts_with("/head/") && !name[6..].contains('/')
}
/// Fuse inference BatchNorm only when its Conv output has exactly one consumer.
pub fn fold_batch_norm(g: &mut GraphProto) -> Result<()> {
    let mut counts = BTreeMap::new();
    for node in &g.node {
        for input in &node.input {
            *counts.entry(input.clone()).or_insert(0usize) += 1;
        }
    }
    for output in &g.output {
        *counts.entry(output.name.clone()).or_insert(0) += 1;
    }
    let mut remove = Vec::new();
    for i in 0..g.node.len() {
        let bn = &g.node[i];
        if bn.op_type != "BatchNormalization" || bn.input.len() < 5 || bn.output.len() != 1 {
            continue;
        }
        if bn
            .attribute
            .iter()
            .any(|a| a.name == "training_mode" && a.i != 0)
        {
            return Err("cannot quantize training-mode BatchNorm".into());
        }
        let Some(conv_idx) = g.node[..i]
            .iter()
            .position(|n| n.op_type == "Conv" && n.output == [bn.input[0].clone()])
        else {
            continue;
        };
        if counts.get(&bn.input[0]).copied() != Some(1) {
            continue;
        }
        let conv = &g.node[conv_idx];
        if conv.input.len() < 2 {
            continue;
        }
        let lookup = |name: &str| -> Result<&TensorProto> {
            g.initializer
                .iter()
                .find(|t| t.name == name)
                .ok_or_else(|| format!("initializer missing {name}").into())
        };
        let weight = lookup(&conv.input[1])?;
        let dims = weight.dims.clone();
        let mut data = floats(weight)?;
        let channels = *dims.first().ok_or("Conv weight has no dimensions")? as usize;
        if channels == 0 {
            return Err("zero-channel convolution".into());
        }
        let per = data.len() / channels;
        let gamma = floats(lookup(&bn.input[1])?)?;
        let beta = floats(lookup(&bn.input[2])?)?;
        let mean = floats(lookup(&bn.input[3])?)?;
        let variance = floats(lookup(&bn.input[4])?)?;
        if [gamma.len(), beta.len(), mean.len(), variance.len()]
            .iter()
            .any(|&n| n != channels)
        {
            return Err("BatchNorm channel mismatch".into());
        }
        let mut bias = if conv.input.len() > 2 {
            floats(lookup(&conv.input[2])?)?
        } else {
            vec![0.; channels]
        };
        if bias.len() != channels {
            return Err("Conv bias channel mismatch".into());
        }
        let eps = bn
            .attribute
            .iter()
            .find(|a| a.name == "epsilon")
            .map(|a| a.f)
            .unwrap_or(0.00001);
        for c in 0..channels {
            if variance[c] + eps <= 0. {
                return Err("invalid BatchNorm variance".into());
            }
            let scale = gamma[c] / (variance[c] + eps).sqrt();
            for v in &mut data[c * per..(c + 1) * per] {
                *v *= scale;
            }
            bias[c] = (bias[c] - mean[c]) * scale + beta[c];
        }
        let output = bn.output[0].clone();
        let weight_name = format!("{}/fused_weight", conv.name);
        let bias_name = format!("{}/fused_bias", conv.name);
        g.initializer.push(float_tensor(&weight_name, &dims, &data));
        g.initializer
            .push(float_tensor(&bias_name, &[channels as i64], &bias));
        g.node[conv_idx].input.truncate(1);
        g.node[conv_idx].input.extend([weight_name, bias_name]);
        g.node[conv_idx].output = vec![output];
        remove.push(i);
    }
    for &i in remove.iter().rev() {
        g.node.remove(i);
    }
    Ok(())
}
pub fn quantize(
    input: &Path,
    output: &Path,
    root: &Path,
    split: &str,
    height: usize,
    width: usize,
    every: usize,
) -> Result<()> {
    crate::graph::protect_input(output, input)?;
    crate::graph::reject_restricted_model(input)?;
    if input == output
        || every == 0
        || height == 0
        || width == 0
        || !height.is_multiple_of(32)
        || !width.is_multiple_of(32)
    {
        return Err(
            "quantization requires different output, positive --every and H,W divisible by 32"
                .into(),
        );
    }
    let paths = training::entries(root, split)?
        .into_iter()
        .step_by(every)
        .collect::<Vec<_>>();
    if paths.is_empty() {
        return Err("calibration split is empty".into());
    }
    crate::graph::protect_input(output, &root.join(format!("{split}.txt")))?;
    for path in &paths {
        crate::graph::protect_input(output, path)?;
    }
    let mut model = ModelProto::parse_from_bytes(&std::fs::read(input)?)?;
    let g = model.graph.as_mut().ok_or("ONNX model has no graph")?;
    if g.node
        .iter()
        .any(|n| n.op_type == "QuantizeLinear" || n.op_type == "DequantizeLinear")
    {
        return Err("model is already quantized".into());
    }
    if g.input.len() != 1 {
        return Err("calibration requires one image input".into());
    }
    if g.initializer.iter().any(|t| !t.external_data.is_empty()) {
        return Err("external ONNX tensor data must be embedded before calibration".into());
    }
    fold_batch_norm(g)?;
    let input_name = g.input[0].name.clone();
    let targets = g
        .node
        .iter()
        .filter(|n| {
            !excluded(&n.name)
                && [
                    "Conv",
                    "BatchNormalization",
                    "Mul",
                    "Add",
                    "Concat",
                    "MaxPool",
                    "Resize",
                    "Sigmoid",
                ]
                .contains(&n.op_type.as_str())
        })
        .flat_map(|n| n.output.clone())
        .collect::<Vec<_>>();
    if targets.is_empty() {
        return Err("no quantizable detector nodes".into());
    }
    let original_outputs = g.output.clone();
    let mut ranges = BTreeMap::<String, (f32, f32)>::new();
    load_runtime()?;
    // Limit retained intermediate tensors: calibrate in groups of eight outputs.
    for chunk in targets.chunks(8) {
        let mut instrumented = model.clone();
        let graph = instrumented.graph.as_mut().unwrap();
        graph.output = chunk.iter().map(|n| value_info(n, &[], false)).collect();
        for info in &mut graph.output {
            info.type_.as_mut().unwrap().mut_tensor_type().shape.clear();
        }
        let mut session = ort::session::Session::builder()?
            .with_intra_threads(1)?
            .with_inter_threads(1)?
            .commit_from_memory(&instrumented.write_to_bytes()?)?;
        for path in &paths {
            let (data, _) = training::letterbox(path, height, width)?;
            update_range(&mut ranges, &input_name, &data)?;
            let tensor = ort::value::Tensor::from_array(([1usize, 3, height, width], data))?;
            let outputs = session.run(ort::inputs![tensor])?;
            for (idx, name) in chunk.iter().enumerate() {
                let (_, data) = outputs[idx].try_extract_tensor::<f32>()?;
                update_range(&mut ranges, name, data)?;
            }
        }
    }
    let g = model.graph.as_mut().unwrap();
    g.output = original_outputs;
    let original_nodes = std::mem::take(&mut g.node);
    let mut rewritten = BTreeMap::new();
    insert_activation(g, &input_name, ranges[&input_name], &mut rewritten);
    let mut weights = BTreeMap::<String, String>::new();
    for mut node in original_nodes {
        for name in &mut node.input {
            if let Some(replacement) = rewritten.get(name) {
                *name = replacement.clone();
            }
        }
        if node.op_type == "Conv" && !excluded(&node.name) && node.input.len() >= 2 {
            let name = node.input[1].clone();
            let replacement = if let Some(v) = weights.get(&name) {
                v.clone()
            } else {
                let initializer = g
                    .initializer
                    .iter()
                    .find(|t| t.name == name)
                    .ok_or("Conv weight not an initializer")?
                    .clone();
                let data = floats(&initializer)?;
                let channels = initializer.dims[0] as usize;
                if channels == 0 || data.len() % channels != 0 {
                    return Err("invalid convolution channels".into());
                }
                let per = data.len() / channels;
                let scales = data
                    .chunks(per)
                    .map(|chunk| {
                        (chunk.iter().map(|v| v.abs()).fold(0f32, f32::max) / 127.).max(1e-12)
                    })
                    .collect::<Vec<_>>();
                let quantized = data
                    .iter()
                    .enumerate()
                    .map(|(i, v)| (v / scales[i / per]).round().clamp(-127., 127.) as i8 as u8)
                    .collect::<Vec<_>>();
                let q = format!("{name}/int8");
                let s = format!("{name}/scale");
                let z = format!("{name}/zero");
                let dq = format!("{name}/dequantized");
                g.initializer.push(TensorProto {
                    name: q.clone(),
                    dims: initializer.dims,
                    data_type: 3,
                    raw_data: quantized,
                    ..Default::default()
                });
                g.initializer
                    .push(float_tensor(&s, &[channels as i64], &scales));
                g.initializer.push(TensorProto {
                    name: z.clone(),
                    dims: vec![channels as i64],
                    data_type: 3,
                    raw_data: vec![0; channels],
                    ..Default::default()
                });
                emit(
                    g,
                    "DequantizeLinear",
                    vec![q, s, z],
                    &dq,
                    vec![int("axis", 0)],
                );
                weights.insert(name, dq.clone());
                dq
            };
            node.input[1] = replacement;
        }
        let outputs = node.output.clone();
        g.node.push(node);
        for name in outputs {
            if let Some(&range) = ranges.get(&name) {
                insert_activation(g, &name, range, &mut rewritten);
            }
        }
    }
    // Remove unused float weights and BatchNorm constants after folding/quantizing.
    let used = g
        .node
        .iter()
        .flat_map(|n| n.input.iter())
        .chain(g.output.iter().map(|v| &v.name))
        .collect::<std::collections::BTreeSet<_>>();
    g.initializer.retain(|t| used.contains(&t.name));
    let bytes = model.write_to_bytes()?;
    // Validate the graph and run a calibration image before publishing a result.
    let mut session = ort::session::Session::builder()?
        .with_intra_threads(1)?
        .with_inter_threads(1)?
        .commit_from_memory(&bytes)?;
    let (data, _) = training::letterbox(&paths[0], height, width)?;
    let tensor = ort::value::Tensor::from_array(([1usize, 3, height, width], data))?;
    let outputs = session.run(ort::inputs![tensor])?;
    let (_, data) = outputs[0].try_extract_tensor::<f32>()?;
    if data.iter().any(|v| !v.is_finite()) {
        return Err("quantized detector produced non-finite predictions".into());
    }
    atomic_write(output, &bytes)?;
    println!(
        "Wrote {}: {} calibration images, {} activation ranges; prediction heads remain FLOAT",
        output.display(),
        paths.len(),
        ranges.len()
    );
    Ok(())
}
fn update_range(ranges: &mut BTreeMap<String, (f32, f32)>, name: &str, data: &[f32]) -> Result<()> {
    if data.is_empty() || data.iter().any(|v| !v.is_finite()) {
        return Err(format!("invalid calibration output {name}").into());
    }
    let range = ranges.entry(name.into()).or_insert((0., 0.));
    for &v in data {
        range.0 = range.0.min(v);
        range.1 = range.1.max(v);
    }
    Ok(())
}
fn insert_activation(
    g: &mut GraphProto,
    name: &str,
    (min, max): (f32, f32),
    rewritten: &mut BTreeMap<String, String>,
) {
    let scale = ((max - min) / 255.).max(1e-12);
    let zero = (-min / scale).round().clamp(0., 255.) as u8;
    let s = format!("{name}/activation_scale");
    let z = format!("{name}/activation_zero");
    let q = format!("{name}/quantized");
    let dq = format!("{name}/dequantized");
    g.initializer.push(float_tensor(&s, &[], &[scale]));
    g.initializer.push(TensorProto {
        name: z.clone(),
        data_type: 2,
        raw_data: vec![zero],
        ..Default::default()
    });
    emit(
        g,
        "QuantizeLinear",
        vec![name.into(), s.clone(), z.clone()],
        &q,
        vec![],
    );
    emit(g, "DequantizeLinear", vec![q, s, z], &dq, vec![]);
    rewritten.insert(name.into(), dq);
}
