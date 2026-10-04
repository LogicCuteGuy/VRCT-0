use crate::{
    graph::Network,
    training::{self, BoxLabel},
    Result,
};
use candle_core::Tensor;
use serde::Serialize;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::Instant,
};

#[derive(Clone, Debug, Serialize)]
pub struct Detection {
    pub bbox: [f32; 4],
    pub score: f32,
}
#[derive(Debug, Serialize)]
pub struct Report {
    pub images: usize,
    pub matched: usize,
    pub missed: usize,
    pub extra: usize,
    pub mean_iou: Option<f32>,
    pub mean_ms: f64,
    pub median_ms: f64,
    pub ap50: f32,
    pub ap75: f32,
    pub ap50_95: f32,
    pub sessions: BTreeMap<String, [usize; 3]>,
}
pub fn decode(rows: &[Vec<f32>], scale: f32, conf: f32, nms: f32) -> Result<Vec<Detection>> {
    if !scale.is_finite()
        || scale <= 0.
        || ![conf, nms]
            .iter()
            .all(|v| v.is_finite() && (0. ..=1.).contains(v))
    {
        return Err("invalid detection thresholds".into());
    }
    let mut candidates = Vec::new();
    for row in rows {
        if row.len() != 6 || row.iter().any(|v| !v.is_finite()) || row[2] < 0. || row[3] < 0. {
            return Err("detector must output finite [cx,cy,w,h,obj,cls]".into());
        }
        let score = row[4] * row[5];
        if score < conf {
            continue;
        }
        candidates.push(Detection {
            bbox: [
                (row[0] - row[2] / 2.) / scale,
                (row[1] - row[3] / 2.) / scale,
                (row[0] + row[2] / 2.) / scale,
                (row[1] + row[3] / 2.) / scale,
            ],
            score,
        });
    }
    candidates.sort_by(|a, b| b.score.total_cmp(&a.score));
    let mut kept: Vec<Detection> = Vec::new();
    for candidate in candidates {
        if kept
            .iter()
            .all(|prior| training::overlap(prior.bbox, candidate.bbox) <= nms)
        {
            kept.push(candidate);
        }
    }
    Ok(kept)
}
pub fn evaluate_network(
    net: &Network,
    paths: &[PathBuf],
    size: usize,
    conf: f32,
    nms: f32,
) -> Result<Report> {
    evaluate(paths, conf, nms, 0.5, |path| {
        let (input, scale) = training::letterbox(path, size, size)?;
        let x = Tensor::from_vec(input, (1, 3, size, size), &net.device)?;
        let now = Instant::now();
        let prediction = net.forward(&x, false)?.to_vec3::<f32>()?;
        Ok((
            prediction
                .into_iter()
                .next()
                .ok_or("empty detector output")?,
            scale,
            now.elapsed().as_secs_f64() * 1000.,
        ))
    })
}
pub fn load_runtime() -> Result<()> {
    let library = std::env::var_os("ORT_DYLIB_PATH")
        .map(PathBuf::from)
        .or_else(|| {
            let name = if cfg!(windows) {
                "onnxruntime.dll"
            } else if cfg!(target_os = "macos") {
                "libonnxruntime.dylib"
            } else {
                "libonnxruntime.so"
            };
            let path = std::env::current_exe().ok()?.parent()?.join(name);
            if path.is_file() {
                Some(path)
            } else {
                Some(
                    Path::new(env!("CARGO_MANIFEST_DIR"))
                        .join("../../resources/onnxruntime")
                        .join(name),
                )
            }
        })
        .ok_or("ONNX Runtime not found; set ORT_DYLIB_PATH")?;
    if !library.is_file() {
        return Err(format!("ONNX Runtime not found: {}", library.display()).into());
    }
    ort::init_from(library)?.commit();
    Ok(())
}
pub fn evaluate_onnx(
    model: &Path,
    root: &Path,
    split: &str,
    size: &str,
    conf: f32,
    nms: f32,
    match_iou: f32,
) -> Result<Report> {
    crate::graph::reject_restricted_model(model)?;
    load_runtime()?;
    let mut session = ort::session::Session::builder()?
        .with_intra_threads(1)?
        .with_inter_threads(1)?
        .commit_from_file(model)?;
    let paths = training::entries(root, split)?;
    let dimensions = parse_size(size)?;
    evaluate(&paths, conf, nms, match_iou, |path| {
        let (original_w, original_h) = image::image_dimensions(path)?;
        let (h, w) = if dimensions.len() == 1 {
            let s = dimensions[0] as f32 / original_w.max(original_h) as f32;
            (
                ((original_h as f32 * s) as usize).max(1).div_ceil(32) * 32,
                ((original_w as f32 * s) as usize).max(1).div_ceil(32) * 32,
            )
        } else {
            (dimensions[0], dimensions[1])
        };
        let (data, scale) = training::letterbox(path, h, w)?;
        let tensor = ort::value::Tensor::from_array(([1usize, 3, h, w], data))?;
        let now = Instant::now();
        let outputs = session.run(ort::inputs![tensor])?;
        let (shape, values) = outputs[0].try_extract_tensor::<f32>()?;
        if shape.len() != 3 || shape[0] != 1 || shape[2] != 6 {
            return Err(format!("unsupported detector output shape {shape:?}").into());
        }
        Ok((
            values.chunks_exact(6).map(<[f32]>::to_vec).collect(),
            scale,
            now.elapsed().as_secs_f64() * 1000.,
        ))
    })
}
pub fn parse_size(value: &str) -> Result<Vec<usize>> {
    let values = value
        .split(',')
        .map(str::parse::<usize>)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    if !(1..=2).contains(&values.len()) || values.iter().any(|&v| v == 0 || v > 16384) {
        return Err("size must be a positive long-edge or H,W, at most 16384".into());
    }
    Ok(values)
}
fn evaluate(
    paths: &[PathBuf],
    conf: f32,
    nms: f32,
    match_iou: f32,
    mut infer: impl FnMut(&Path) -> Result<(Vec<Vec<f32>>, f32, f64)>,
) -> Result<Report> {
    if paths.is_empty() {
        return Err("evaluation split is empty".into());
    }
    if !match_iou.is_finite() || !(0. ..=1.).contains(&match_iou) {
        return Err("invalid match IoU".into());
    }
    let mut matched = 0;
    let mut missed = 0;
    let mut extra = 0;
    let mut ious = Vec::new();
    let mut latencies = Vec::new();
    let mut sessions = BTreeMap::new();
    let mut all_truth = Vec::new();
    let mut all_pred = Vec::new();
    for path in paths {
        let (rows, scale, ms) = infer(path)?;
        let predictions = decode(&rows, scale, conf, nms)?;
        let truth = training::labels(path)?;
        latencies.push(ms);
        let mut used = vec![false; predictions.len()];
        let mut hits = 0;
        for gt in &truth {
            let best = predictions
                .iter()
                .enumerate()
                .filter(|(i, _)| !used[*i])
                .map(|(i, p)| (i, training::overlap(gt.xyxy(), p.bbox)))
                .filter(|(_, iou)| *iou >= match_iou)
                .max_by(|a, b| a.1.total_cmp(&b.1));
            if let Some((i, iou)) = best {
                used[i] = true;
                matched += 1;
                hits += 1;
                ious.push(iou);
            } else {
                missed += 1;
            }
        }
        let extras = predictions.len() - hits;
        extra += extras;
        let name = path
            .parent()
            .and_then(Path::parent)
            .and_then(Path::file_name)
            .ok_or("session missing")?
            .to_string_lossy()
            .to_string();
        let counters = sessions.entry(name).or_insert([0usize; 3]);
        counters[0] += hits;
        counters[1] += truth.len();
        counters[2] += extras;
        all_truth.push(truth);
        all_pred.push(predictions);
    }
    let aps = (0..10)
        .map(|i| average_precision(&all_truth, &all_pred, 0.5 + i as f32 * 0.05))
        .collect::<Vec<_>>();
    latencies.sort_by(f64::total_cmp);
    let count = latencies.len();
    let median = if count % 2 == 0 {
        (latencies[count / 2 - 1] + latencies[count / 2]) / 2.
    } else {
        latencies[count / 2]
    };
    Ok(Report {
        images: paths.len(),
        matched,
        missed,
        extra,
        mean_iou: (!ious.is_empty()).then(|| ious.iter().sum::<f32>() / ious.len() as f32),
        mean_ms: latencies.iter().sum::<f64>() / count as f64,
        median_ms: median,
        ap50: aps[0],
        ap75: aps[5],
        ap50_95: aps.iter().sum::<f32>() / 10.,
        sessions,
    })
}
/// One-class COCO-style 101-point interpolation, max 100 detections per image.
/// Dataset annotations contain no crowds; every verified empty image remains a negative.
pub fn average_precision(
    truth: &[Vec<BoxLabel>],
    predictions: &[Vec<Detection>],
    threshold: f32,
) -> f32 {
    let total: usize = truth.iter().map(Vec::len).sum();
    if total == 0 {
        return 0.;
    }
    let mut candidates = predictions
        .iter()
        .enumerate()
        .flat_map(|(image, p)| {
            let mut p = p.iter().collect::<Vec<_>>();
            p.sort_by(|a, b| b.score.total_cmp(&a.score));
            p.into_iter()
                .take(100)
                .map(move |p| (image, p))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|a, b| b.1.score.total_cmp(&a.1.score));
    let mut used = truth
        .iter()
        .map(|v| vec![false; v.len()])
        .collect::<Vec<_>>();
    let mut points = Vec::new();
    let mut tp = 0f32;
    let mut fp = 0f32;
    for (image, p) in candidates {
        let best = truth[image]
            .iter()
            .enumerate()
            .filter(|(i, _)| !used[image][*i])
            .map(|(i, t)| (i, training::overlap(t.xyxy(), p.bbox)))
            .filter(|(_, iou)| *iou >= threshold)
            .max_by(|a, b| a.1.total_cmp(&b.1));
        if let Some((i, _)) = best {
            used[image][i] = true;
            tp += 1.;
        } else {
            fp += 1.;
        }
        points.push((tp / total as f32, tp / (tp + fp)));
    }
    (0..=100)
        .map(|r| {
            points
                .iter()
                .filter(|(recall, _)| *recall >= r as f32 / 100.)
                .map(|p| p.1)
                .fold(0f32, f32::max)
        })
        .sum::<f32>()
        / 101.
}
