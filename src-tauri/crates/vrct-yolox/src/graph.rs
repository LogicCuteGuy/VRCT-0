use crate::Result;
use candle_core::{DType, Device, Tensor, Var};
use onnx_protobuf::{attribute_proto::AttributeType as A, *};
use protobuf::{EnumOrUnknown, MessageField};
use rand::Rng;
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Clone, Debug)]
pub enum Op {
    Conv {
        weight: String,
        bias: Option<String>,
        stride: usize,
        groups: usize,
    },
    BatchNorm {
        name: String,
    },
    Silu,
    Add,
    Cat,
    Slice {
        y: usize,
        x: usize,
    },
    Pool {
        kernel: usize,
    },
    Upsample,
    Head {
        stride: usize,
    },
    Outputs,
}
#[derive(Clone, Debug)]
pub struct Node {
    pub name: String,
    pub inputs: Vec<usize>,
    pub op: Op,
}
pub struct Network {
    pub device: Device,
    pub variables: BTreeMap<String, Var>,
    pub nodes: Vec<Node>,
    pub output: usize,
    pub variant: String,
    pub mixed_precision: bool,
}
impl Network {
    pub fn parameter(
        &mut self,
        name: &str,
        shape: &[usize],
        rng: &mut impl Rng,
        value: Option<f32>,
    ) -> Result<()> {
        let count: usize = shape.iter().product();
        let bound = 1.0 / (shape.iter().skip(1).product::<usize>().max(1) as f32).sqrt();
        let data = (0..count)
            .map(|_| value.unwrap_or_else(|| rng.random_range(-bound..bound)))
            .collect::<Vec<_>>();
        self.variables
            .insert(name.into(), Var::from_vec(data, shape, &self.device)?);
        Ok(())
    }
    pub fn node(&mut self, name: impl Into<String>, inputs: &[usize], op: Op) -> usize {
        self.nodes.push(Node {
            name: name.into(),
            inputs: inputs.to_vec(),
            op,
        });
        self.nodes.len() // input tensor has index 0
    }
    pub fn forward(&self, input: &Tensor, training: bool) -> Result<Tensor> {
        Ok(self.run(input, training)?.remove(self.output))
    }
    pub fn run(&self, input: &Tensor, training: bool) -> Result<Vec<Tensor>> {
        if self.mixed_precision && !self.device.is_cuda() {
            return Err("FP16 convolution training requires CUDA".into());
        }
        let mut values = vec![input.clone()];
        for node in &self.nodes {
            let x = &values[node.inputs[0]];
            let y = match &node.op {
                Op::Conv {
                    weight,
                    bias,
                    stride,
                    groups,
                } => {
                    // Keep master parameters, BatchNorm and decoded losses in F32.
                    // Cast only convolutions; gradients return through the casts.
                    let dtype = if self.mixed_precision {
                        DType::F16
                    } else {
                        DType::F32
                    };
                    let conv_input = x.to_dtype(dtype)?;
                    let x = &conv_input;
                    let w = self.variables[weight].to_dtype(dtype)?;
                    let padding = w.dim(2)? / 2;
                    let mut y = if *groups == 1 {
                        x.conv2d(&w, padding, *stride, 1, 1)?
                    } else {
                        // Split depthwise groups so all constituent convolutions have a backward path.
                        let input_channels = x.dim(1)? / groups;
                        let output_channels = w.dim(0)? / groups;
                        let parts = (0..*groups)
                            .map(|g| {
                                x.narrow(1, g * input_channels, input_channels)?
                                    .contiguous()?
                                    .conv2d(
                                        &w.narrow(0, g * output_channels, output_channels)?
                                            .contiguous()?,
                                        padding,
                                        *stride,
                                        1,
                                        1,
                                    )
                            })
                            .collect::<candle_core::Result<Vec<_>>>()?;
                        Tensor::cat(&parts, 1)?
                    };
                    y = y.to_dtype(DType::F32)?;
                    if let Some(bias) = bias {
                        y = y.broadcast_add(&self.variables[bias].reshape((
                            1,
                            w.dim(0)?,
                            1,
                            1,
                        ))?)?;
                    }
                    y
                }
                Op::BatchNorm { name } => {
                    let c = x.dim(1)?;
                    let shape = (1, c, 1, 1);
                    let mean_name = format!("{name}.running_mean");
                    let var_name = format!("{name}.running_var");
                    let (mean, variance) = if training {
                        let mean = x.mean_keepdim((0, 2, 3))?;
                        let variance = x.broadcast_sub(&mean)?.sqr()?.mean_keepdim((0, 2, 3))?;
                        let n = x.elem_count() / c;
                        let unbiased = if n > 1 {
                            variance.affine(n as f64 / (n - 1) as f64, 0.)?
                        } else {
                            variance.clone()
                        };
                        self.variables[&mean_name].set(
                            &(&self.variables[&mean_name].affine(0.97, 0.)?
                                + mean.flatten_all()?.detach().affine(0.03, 0.)?)?,
                        )?;
                        self.variables[&var_name].set(
                            &(&self.variables[&var_name].affine(0.97, 0.)?
                                + unbiased.flatten_all()?.detach().affine(0.03, 0.)?)?,
                        )?;
                        (mean, variance)
                    } else {
                        (
                            self.variables[&mean_name].reshape(shape)?,
                            self.variables[&var_name].reshape(shape)?,
                        )
                    };
                    x.broadcast_sub(&mean)?
                        .broadcast_div(&variance.affine(1., 0.001)?.sqrt()?)?
                        .broadcast_mul(&self.variables[&format!("{name}.weight")].reshape(shape)?)?
                        .broadcast_add(&self.variables[&format!("{name}.bias")].reshape(shape)?)?
                }
                Op::Silu => x.broadcast_div(&x.neg()?.exp()?.affine(1., 1.)?)?,
                Op::Add => (x + &values[node.inputs[1]])?,
                Op::Cat => Tensor::cat(
                    &node.inputs.iter().map(|&i| &values[i]).collect::<Vec<_>>(),
                    1,
                )?,
                Op::Slice { y, x: offset } => {
                    let h: Vec<u32> = (*y..x.dim(2)?).step_by(2).map(|v| v as u32).collect();
                    let w: Vec<u32> = (*offset..x.dim(3)?).step_by(2).map(|v| v as u32).collect();
                    x.index_select(&Tensor::new(h, &self.device)?, 2)?
                        .index_select(&Tensor::new(w, &self.device)?, 3)?
                }
                Op::Pool { kernel } => max_pool_same(x, *kernel)?,
                Op::Upsample => x.upsample_nearest2d(x.dim(2)? * 2, x.dim(3)? * 2)?,
                Op::Head { stride } => {
                    let (b, _, h, w) = x.dims4()?;
                    let raw = x.permute((0, 2, 3, 1))?.reshape((b, h * w, 6))?;
                    let grid = (0..h)
                        .flat_map(|y| (0..w).flat_map(move |x| [x as f32, y as f32]))
                        .collect::<Vec<_>>();
                    let xy = raw
                        .narrow(2, 0, 2)?
                        .broadcast_add(&Tensor::from_vec(grid, (1, h * w, 2), &self.device)?)?
                        .affine(*stride as f64, 0.)?;
                    let wh = raw.narrow(2, 2, 2)?.exp()?.affine(*stride as f64, 0.)?;
                    let scores = raw.narrow(2, 4, 2)?;
                    let scores = if training {
                        scores
                    } else {
                        scores.neg()?.exp()?.affine(1., 1.)?.recip()?
                    };
                    Tensor::cat(&[xy, wh, scores], 2)?
                }
                Op::Outputs => Tensor::cat(
                    &node.inputs.iter().map(|&i| &values[i]).collect::<Vec<_>>(),
                    1,
                )?,
            };
            values.push(y);
        }
        Ok(values)
    }
    pub fn save(&self, path: &Path) -> Result<()> {
        let values = self
            .variables
            .iter()
            .map(|(name, var)| (name.clone(), var.as_detached_tensor()))
            .collect::<std::collections::HashMap<_, _>>();
        candle_core::safetensors::save(&values, path)?;
        Ok(())
    }
    pub fn load(&self, path: &Path, pretrained: bool) -> Result<usize> {
        reject_restricted_model(path)?;
        let tensors = if path.extension().and_then(|s| s.to_str()) == Some("pth") {
            candle_core::pickle::read_all_with_key(path, Some("model"))?
                .into_iter()
                .collect::<BTreeMap<_, _>>()
        } else {
            candle_core::safetensors::load(path, &self.device)?
                .into_iter()
                .collect()
        };
        // Validate the entire checkpoint before mutating a live network.
        let mut ready = Vec::new();
        for (name, var) in &self.variables {
            match tensors.get(name) {
                Some(t) if t.dims() == var.dims() => {
                    let tensor = t.to_device(&self.device)?.to_dtype(DType::F32)?;
                    if tensor
                        .flatten_all()?
                        .to_vec1::<f32>()?
                        .iter()
                        .any(|v| !v.is_finite())
                    {
                        return Err(format!("checkpoint has non-finite parameter {name}").into());
                    }
                    ready.push((var, tensor));
                }
                _ if pretrained && name.contains("cls_preds") => {}
                _ => return Err(format!("checkpoint missing or wrong shape for {name}").into()),
            }
        }
        let loaded = ready.len();
        for (var, tensor) in ready {
            var.set(&tensor)?;
        }
        Ok(loaded)
    }
    pub fn export(&self, path: &Path, height: usize, width: usize, dynamic: bool) -> Result<()> {
        if height == 0 || width == 0 || !height.is_multiple_of(32) || !width.is_multiple_of(32) {
            return Err("export dimensions must be positive multiples of 32".into());
        }
        let mut g = GraphProto {
            name: format!("vrct-yolox-{}", self.variant),
            ..Default::default()
        };
        g.input.push(value_info(
            "images",
            &[1, 3, height as i64, width as i64],
            dynamic,
        ));
        for (name, var) in &self.variables {
            let values = var.flatten_all()?.to_vec1::<f32>()?;
            g.initializer.push(float_tensor(
                name,
                &var.dims().iter().map(|&v| v as i64).collect::<Vec<_>>(),
                &values,
            ));
        }
        let mut names = vec!["images".to_string()];
        for node in &self.nodes {
            let output = node.name.clone();
            let x = names[node.inputs[0]].clone();
            match &node.op {
                Op::Conv {
                    weight,
                    bias,
                    stride,
                    groups,
                } => {
                    let w = &self.variables[weight];
                    let p = w.dim(2)? as i64 / 2;
                    let mut inputs = vec![x, weight.clone()];
                    if let Some(b) = bias {
                        inputs.push(b.clone());
                    }
                    emit(
                        &mut g,
                        "Conv",
                        inputs,
                        &output,
                        vec![
                            ints("strides", &[*stride as i64; 2]),
                            ints("pads", &[p; 4]),
                            int("group", *groups as i64),
                        ],
                    );
                }
                Op::BatchNorm { name } => emit(
                    &mut g,
                    "BatchNormalization",
                    vec![
                        x,
                        format!("{name}.weight"),
                        format!("{name}.bias"),
                        format!("{name}.running_mean"),
                        format!("{name}.running_var"),
                    ],
                    &output,
                    vec![float("epsilon", 0.001)],
                ),
                Op::Silu => {
                    let s = format!("{output}/sigmoid");
                    emit(&mut g, "Sigmoid", vec![x.clone()], &s, vec![]);
                    emit(&mut g, "Mul", vec![x, s], &output, vec![]);
                }
                Op::Add => emit(
                    &mut g,
                    "Add",
                    node.inputs.iter().map(|&i| names[i].clone()).collect(),
                    &output,
                    vec![],
                ),
                Op::Cat | Op::Outputs => emit(
                    &mut g,
                    "Concat",
                    node.inputs.iter().map(|&i| names[i].clone()).collect(),
                    &output,
                    vec![int("axis", 1)],
                ),
                Op::Slice { y, x: offset } => {
                    let constants = [
                        ("starts", vec![*y as i64, *offset as i64]),
                        ("ends", vec![i64::MAX, i64::MAX]),
                        ("axes", vec![2, 3]),
                        ("steps", vec![2, 2]),
                    ];
                    let mut inputs = vec![x];
                    for (suffix, data) in constants {
                        let n = format!("{output}/{suffix}");
                        g.initializer.push(int_tensor(&n, &[2], &data));
                        inputs.push(n);
                    }
                    emit(&mut g, "Slice", inputs, &output, vec![]);
                }
                Op::Pool { kernel } => emit(
                    &mut g,
                    "MaxPool",
                    vec![x],
                    &output,
                    vec![
                        ints("kernel_shape", &[*kernel as i64; 2]),
                        ints("strides", &[1, 1]),
                        ints("pads", &[(*kernel / 2) as i64; 4]),
                    ],
                ),
                Op::Upsample => {
                    let s = format!("{output}/scales");
                    g.initializer
                        .push(float_tensor(&s, &[4], &[1., 1., 2., 2.]));
                    emit(
                        &mut g,
                        "Resize",
                        vec![x, String::new(), s],
                        &output,
                        vec![
                            string("mode", "nearest"),
                            string("coordinate_transformation_mode", "asymmetric"),
                            string("nearest_mode", "floor"),
                        ],
                    );
                }
                Op::Head { stride } => export_head(&mut g, &x, &output, *stride)?,
            }
            names.push(output);
        }
        emit(
            &mut g,
            "Identity",
            vec![names[self.output].clone()],
            "output",
            vec![],
        );
        g.output.push(value_info("output", &[1, -1, 6], false));
        let model = ModelProto {
            ir_version: 9,
            producer_name: "VRCT Rust YOLOX".into(),
            opset_import: vec![OperatorSetIdProto {
                version: 17,
                ..Default::default()
            }],
            graph: MessageField::some(g),
            ..Default::default()
        };
        atomic_write(path, &model.write_to_bytes()?)?;
        Ok(())
    }
}
fn max_pool_same(x: &Tensor, k: usize) -> Result<Tensor> {
    let (b, c, h, w) = x.dims4()?;
    let p = k / 2;
    let side = Tensor::full(-f32::MAX, (b, c, h, p), x.device())?;
    let padded = Tensor::cat(&[&side, x, &side], 3)?;
    let edge = Tensor::full(-f32::MAX, (b, c, p, w + 2 * p), x.device())?;
    let padded = Tensor::cat(&[&edge, &padded, &edge], 2)?;
    let mut out = padded.narrow(2, 0, h)?.narrow(3, 0, w)?;
    for y in 0..k {
        for z in 0..k {
            if y + z > 0 {
                let candidate = padded.narrow(2, y, h)?.narrow(3, z, w)?;
                out = candidate.gt(&out)?.where_cond(&candidate, &out)?;
            }
        }
    }
    Ok(out)
}
pub fn reject_restricted_model(path: &Path) -> Result<()> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let fingerprint: serde_json::Value =
        serde_json::from_str(include_str!("../../xtask/protected-model-fingerprint.json"))?;
    if std::fs::metadata(path)?.len()
        == fingerprint["bytes"]
            .as_u64()
            .ok_or("invalid restricted-model fingerprint")?
    {
        let mut file = std::fs::File::open(path)?;
        let mut hash = Sha256::new();
        let mut bytes = [0u8; 65536];
        loop {
            let count = file.read(&mut bytes)?;
            if count == 0 {
                break;
            }
            hash.update(&bytes[..count]);
        }
        if hex::encode(hash.finalize())
            == fingerprint["sha256"]
                .as_str()
                .ok_or("invalid restricted-model fingerprint")?
        {
            return Err("restricted VRCT detector cannot be used for training, export, quantization or evaluation".into());
        }
    }
    Ok(())
}
pub fn protect_input(output: &Path, input: &Path) -> Result<()> {
    if output == input
        || (output.exists() && input.exists() && same_file::is_same_file(output, input)?)
    {
        return Err(format!("output would overwrite input {}", input.display()).into());
    }
    Ok(())
}
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)?;
    let mut staged = tempfile::NamedTempFile::new_in(parent)?;
    staged.write_all(bytes)?;
    staged.as_file().sync_all()?;
    staged.persist(path)?;
    Ok(())
}
pub fn float_tensor(name: &str, dims: &[i64], data: &[f32]) -> TensorProto {
    TensorProto {
        name: name.into(),
        dims: dims.to_vec(),
        data_type: 1,
        raw_data: data.iter().flat_map(|v| v.to_le_bytes()).collect(),
        ..Default::default()
    }
}
pub fn int_tensor(name: &str, dims: &[i64], data: &[i64]) -> TensorProto {
    TensorProto {
        name: name.into(),
        dims: dims.to_vec(),
        data_type: 7,
        int64_data: data.to_vec(),
        ..Default::default()
    }
}
pub fn value_info(name: &str, dims: &[i64], dynamic: bool) -> ValueInfoProto {
    let mut shape = TensorShapeProto::new();
    for (i, &v) in dims.iter().enumerate() {
        let mut d = tensor_shape_proto::Dimension::new();
        if v < 0 || dynamic && i >= 2 {
            d.set_dim_param(format!("{name}_dim{i}"));
        } else {
            d.set_dim_value(v);
        }
        shape.dim.push(d);
    }
    let mut t = TypeProto::new();
    t.set_tensor_type(type_proto::Tensor {
        elem_type: 1,
        shape: MessageField::some(shape),
        ..Default::default()
    });
    ValueInfoProto {
        name: name.into(),
        type_: MessageField::some(t),
        ..Default::default()
    }
}
pub fn int(name: &str, v: i64) -> AttributeProto {
    AttributeProto {
        name: name.into(),
        type_: EnumOrUnknown::new(A::INT),
        i: v,
        ..Default::default()
    }
}
pub fn ints(name: &str, v: &[i64]) -> AttributeProto {
    AttributeProto {
        name: name.into(),
        type_: EnumOrUnknown::new(A::INTS),
        ints: v.to_vec(),
        ..Default::default()
    }
}
pub fn float(name: &str, v: f32) -> AttributeProto {
    AttributeProto {
        name: name.into(),
        type_: EnumOrUnknown::new(A::FLOAT),
        f: v,
        ..Default::default()
    }
}
pub fn string(name: &str, v: &str) -> AttributeProto {
    AttributeProto {
        name: name.into(),
        type_: EnumOrUnknown::new(A::STRING),
        s: v.as_bytes().to_vec(),
        ..Default::default()
    }
}
pub fn emit(
    g: &mut GraphProto,
    op: &str,
    inputs: Vec<String>,
    output: &str,
    attrs: Vec<AttributeProto>,
) {
    g.node.push(NodeProto {
        name: output.into(),
        op_type: op.into(),
        input: inputs,
        output: vec![output.into()],
        attribute: attrs,
        ..Default::default()
    });
}
fn export_head(g: &mut GraphProto, x: &str, out: &str, stride: usize) -> Result<()> {
    let name = |suffix: &str| format!("{out}/{suffix}");
    // Build grid from runtime feature-map shape, so --dynamic remains truly dynamic.
    emit(g, "Shape", vec![x.into()], &name("shape"), vec![]);
    for (axis, label) in [(2, "h"), (3, "w")] {
        let index = name(&format!("{label}_index"));
        g.initializer.push(int_tensor(&index, &[], &[axis]));
        emit(
            g,
            "Gather",
            vec![name("shape"), index],
            &name(label),
            vec![int("axis", 0)],
        );
    }
    g.initializer.push(int_tensor(&name("zero"), &[], &[0]));
    g.initializer.push(int_tensor(&name("one"), &[], &[1]));
    g.initializer.push(int_tensor(&name("axis0"), &[1], &[0]));
    g.initializer.push(int_tensor(&name("axis1"), &[1], &[1]));
    for label in ["h", "w"] {
        emit(
            g,
            "Range",
            vec![name("zero"), name(label), name("one")],
            &name(&format!("range_{label}")),
            vec![],
        );
        emit(
            g,
            "Unsqueeze",
            vec![name(label), name("axis0")],
            &name(&format!("{label}_vec")),
            vec![],
        );
    }
    emit(
        g,
        "Concat",
        vec![name("h_vec"), name("w_vec")],
        &name("hw"),
        vec![int("axis", 0)],
    );
    emit(
        g,
        "Unsqueeze",
        vec![name("range_h"), name("axis1")],
        &name("ys"),
        vec![],
    );
    emit(
        g,
        "Unsqueeze",
        vec![name("range_w"), name("axis0")],
        &name("xs"),
        vec![],
    );
    for label in ["x", "y"] {
        emit(
            g,
            "Expand",
            vec![name(&format!("{label}s")), name("hw")],
            &name(&format!("grid_{label}")),
            vec![],
        );
        g.initializer.push(int_tensor(
            &name(&format!("grid_{label}_shape")),
            &[3],
            &[1, -1, 1],
        ));
        emit(
            g,
            "Reshape",
            vec![
                name(&format!("grid_{label}")),
                name(&format!("grid_{label}_shape")),
            ],
            &name(&format!("grid_{label}_flat")),
            vec![],
        );
    }
    emit(
        g,
        "Concat",
        vec![name("grid_x_flat"), name("grid_y_flat")],
        &name("grid_int"),
        vec![int("axis", 2)],
    );
    emit(
        g,
        "Cast",
        vec![name("grid_int")],
        &name("grid"),
        vec![int("to", 1)],
    );
    emit(
        g,
        "Transpose",
        vec![x.into()],
        &name("hwc"),
        vec![ints("perm", &[0, 2, 3, 1])],
    );
    g.initializer
        .push(int_tensor(&name("raw_shape"), &[3], &[1, -1, 6]));
    emit(
        g,
        "Reshape",
        vec![name("hwc"), name("raw_shape")],
        &name("raw"),
        vec![],
    );
    for (label, start, end) in [("xy", 0, 2), ("wh", 2, 4), ("score", 4, 6)] {
        for (suffix, data) in [("start", start), ("end", end), ("axis", 2)] {
            g.initializer.push(int_tensor(
                &name(&format!("{label}_{suffix}")),
                &[1],
                &[data],
            ));
        }
        emit(
            g,
            "Slice",
            vec![
                name("raw"),
                name(&format!("{label}_start")),
                name(&format!("{label}_end")),
                name(&format!("{label}_axis")),
            ],
            &name(label),
            vec![],
        );
    }
    g.initializer
        .push(float_tensor(&name("stride"), &[], &[stride as f32]));
    emit(
        g,
        "Add",
        vec![name("xy"), name("grid")],
        &name("xy_grid"),
        vec![],
    );
    emit(
        g,
        "Mul",
        vec![name("xy_grid"), name("stride")],
        &name("xy_out"),
        vec![],
    );
    emit(g, "Exp", vec![name("wh")], &name("wh_exp"), vec![]);
    emit(
        g,
        "Mul",
        vec![name("wh_exp"), name("stride")],
        &name("wh_out"),
        vec![],
    );
    emit(
        g,
        "Sigmoid",
        vec![name("score")],
        &name("score_out"),
        vec![],
    );
    emit(
        g,
        "Concat",
        vec![name("xy_out"), name("wh_out"), name("score_out")],
        out,
        vec![int("axis", 2)],
    );
    Ok(())
}
