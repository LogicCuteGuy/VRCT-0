use crate::{
    ema::Ema,
    graph::{atomic_write, Network},
    network, Result,
};
use candle_core::{Device, Tensor};
use rand::{seq::SliceRandom, Rng, SeedableRng};
use rand_chacha::ChaCha20Rng;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct BoxLabel {
    pub cx: f32,
    pub cy: f32,
    pub w: f32,
    pub h: f32,
}
impl BoxLabel {
    pub fn xyxy(self) -> [f32; 4] {
        [
            self.cx - self.w / 2.,
            self.cy - self.h / 2.,
            self.cx + self.w / 2.,
            self.cy + self.h / 2.,
        ]
    }
}
pub fn overlap(a: [f32; 4], b: [f32; 4]) -> f32 {
    let intersection =
        (a[2].min(b[2]) - a[0].max(b[0])).max(0.) * (a[3].min(b[3]) - a[1].max(b[1])).max(0.);
    let union = (a[2] - a[0]).max(0.) * (a[3] - a[1]).max(0.)
        + (b[2] - b[0]).max(0.) * (b[3] - b[1]).max(0.)
        - intersection;
    if union > 0. {
        intersection / union
    } else {
        0.
    }
}
pub fn resolve(root: &Path, entry: &str) -> Result<PathBuf> {
    let root = std::fs::canonicalize(root)?;
    let relative = Path::new(entry.trim_start_matches("./"));
    if relative.is_absolute()
        || relative
            .components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)))
    {
        return Err("dataset path must be relative without traversal".into());
    }
    let path = std::fs::canonicalize(root.join(relative))?;
    if !path.starts_with(&root) || !path.is_file() {
        return Err("dataset image escapes root or is not a file".into());
    }
    Ok(path)
}
pub fn entries(root: &Path, split: &str) -> Result<Vec<PathBuf>> {
    if !["train", "val", "val_fixed"].contains(&split) {
        return Err("split must be train, val or val_fixed".into());
    }
    std::fs::read_to_string(root.join(format!("{split}.txt")))?
        .lines()
        .filter(|s| !s.trim().is_empty())
        .map(|s| resolve(root, s.trim()))
        .collect()
}
pub fn labels(path: &Path) -> Result<Vec<BoxLabel>> {
    let label = path
        .parent()
        .and_then(Path::parent)
        .ok_or("image path has no session")?
        .join("labels")
        .join(format!(
            "{}.txt",
            path.file_stem()
                .ok_or("image has no stem")?
                .to_string_lossy()
        ));
    let (w, h) = image::image_dimensions(path)?;
    let mut out = Vec::new();
    for row in std::fs::read_to_string(label)?
        .lines()
        .filter(|s| !s.trim().is_empty())
    {
        let values = row.split_whitespace().collect::<Vec<_>>();
        if values.len() != 5 || values[0] != "0" {
            return Err("label must be class 0 and four normalized coordinates".into());
        }
        let v = values[1..]
            .iter()
            .map(|s| s.parse::<f32>())
            .collect::<std::result::Result<Vec<_>, _>>()?;
        if v.iter().any(|v| !v.is_finite() || !(0. ..=1.).contains(v)) || v[2] <= 0. || v[3] <= 0. {
            return Err("label has invalid coordinates".into());
        }
        if v[0] - v[2] / 2. < -0.00001
            || v[1] - v[3] / 2. < -0.00001
            || v[0] + v[2] / 2. > 1.00001
            || v[1] + v[3] / 2. > 1.00001
        {
            return Err("label extends outside image".into());
        }
        out.push(BoxLabel {
            cx: v[0] * w as f32,
            cy: v[1] * h as f32,
            w: v[2] * w as f32,
            h: v[3] * h as f32,
        });
    }
    Ok(out)
}
pub fn letterbox(path: &Path, height: usize, width: usize) -> Result<(Vec<f32>, f32)> {
    if height == 0 || width == 0 || height > 16384 || width > 16384 {
        return Err("invalid image size".into());
    }
    let image = image::open(path)?.to_rgb8();
    let scale = (width as f32 / image.width() as f32).min(height as f32 / image.height() as f32);
    let w = (image.width() as f32 * scale) as u32;
    let h = (image.height() as f32 * scale) as u32;
    let resized = image::imageops::resize(
        &image,
        w.max(1),
        h.max(1),
        image::imageops::FilterType::Triangle,
    );
    let mut out = vec![114f32; 3 * height * width];
    for (x, y, p) in resized.enumerate_pixels() {
        for c in 0..3 {
            out[c * height * width + y as usize * width + x as usize] = p[2 - c] as f32;
        }
    }
    Ok((out, scale))
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TrainOptions {
    pub root: PathBuf,
    pub output: PathBuf,
    pub variant: String,
    pub checkpoint: Option<PathBuf>,
    pub resume: bool,
    pub epochs: usize,
    pub batch_size: usize,
    pub size: usize,
    pub seed: u64,
    pub lr: f64,
    pub warmup_epochs: usize,
    pub no_aug_epochs: usize,
    pub eval_interval: usize,
    pub device: String,
    #[serde(default)]
    pub fp16: bool,
    pub multiscale_range: usize,
}
#[derive(Serialize, Deserialize)]
struct State {
    epoch: usize,
    step: usize,
    best_ap: f32,
    options: TrainOptions,
    #[serde(default = "initial_loss_scale")]
    loss_scale: f64,
    #[serde(default)]
    scale_successes: usize,
    #[serde(default)]
    ema_updates: usize,
}
fn initial_loss_scale() -> f64 {
    65536.
}
pub fn device(name: &str) -> Result<Device> {
    match name {
        "cpu" => Ok(Device::Cpu),
        "cuda" => Ok(Device::new_cuda(0)?),
        _ => Err("device must be cpu or cuda".into()),
    }
}
pub fn train(options: TrainOptions) -> Result<()> {
    if options.epochs == 0
        || options.batch_size == 0
        || options.size < 64
        || !options.size.is_multiple_of(32)
        || !options.lr.is_finite()
        || options.lr <= 0.
        || options.eval_interval == 0
    {
        return Err("invalid training options".into());
    }
    if options.fp16 && options.device != "cuda" {
        return Err("--fp16 requires --device cuda".into());
    }
    let paths = entries(&options.root, "train")?;
    if paths.is_empty() {
        return Err("training split is empty".into());
    }
    let val = entries(&options.root, "val")?;
    if val.is_empty() {
        return Err("validation split is empty".into());
    }
    let mut net = network::build(&options.variant, device(&options.device)?, options.seed)?;
    net.mixed_precision = options.fp16;
    let mut start = 0;
    let mut step = 0;
    let mut best = -1f32;
    let mut momentum: BTreeMap<String, Tensor> = BTreeMap::new();
    let mut loss_scale = if options.fp16 {
        initial_loss_scale()
    } else {
        1.
    };
    let mut scale_successes = 0;
    let mut ema_updates = 0;
    if options.resume {
        let checkpoint = options
            .checkpoint
            .as_deref()
            .ok_or("--resume requires --ckpt checkpoint directory")?;
        let state: State = serde_json::from_slice(&std::fs::read(checkpoint.join("state.json"))?)?;
        if state.options.variant != options.variant
            || state.options.size != options.size
            || state.options.seed != options.seed
            || state.options.batch_size != options.batch_size
            || state.options.fp16 != options.fp16
        {
            return Err("resume variant, size, seed and batch size must match checkpoint".into());
        }
        let training_weights = checkpoint.join("training.safetensors");
        net.load(
            &if training_weights.is_file() {
                training_weights
            } else {
                checkpoint.join("weights.safetensors")
            },
            false,
        )?;
        momentum =
            candle_core::safetensors::load(checkpoint.join("momentum.safetensors"), &net.device)?
                .into_iter()
                .collect();
        for (name, tensor) in &momentum {
            let variable = net
                .variables
                .get(name)
                .ok_or("optimizer contains unknown parameter")?;
            if tensor.dims() != variable.dims()
                || tensor.dtype() != variable.dtype()
                || tensor
                    .flatten_all()?
                    .to_vec1::<f32>()?
                    .iter()
                    .any(|v| !v.is_finite())
            {
                return Err(format!("invalid optimizer state for {name}").into());
            }
        }
        start = state.epoch;
        step = state.step;
        best = state.best_ap;
        if state.scale_successes >= 2000
            || !state.loss_scale.is_finite()
            || !(1. ..=initial_loss_scale()).contains(&state.loss_scale)
        {
            return Err("invalid checkpoint loss scale".into());
        }
        loss_scale = if options.fp16 { state.loss_scale } else { 1. };
        scale_successes = state.scale_successes;
        ema_updates = state.ema_updates;
    } else if let Some(checkpoint) = &options.checkpoint {
        net.load(checkpoint, true)?;
    }
    let mut ema = if options.resume {
        Ema::load(
            options
                .checkpoint
                .as_ref()
                .ok_or("missing resume checkpoint")?
                .join("weights.safetensors"),
            &net,
            ema_updates,
        )?
    } else {
        Ema::new(&net)?
    };
    let evaluation_net = network::build(&options.variant, net.device.clone(), options.seed)?;
    std::fs::create_dir_all(&options.output)?;
    let mut history: Vec<serde_json::Value> =
        if options.resume && options.output.join("history.json").is_file() {
            serde_json::from_slice(&std::fs::read(options.output.join("history.json"))?)?
        } else {
            Vec::new()
        };
    let batches = paths.len().div_ceil(options.batch_size);
    for epoch in start..options.epochs {
        // Epoch-specific seeds also make resumed sampling reproducible.
        let mut rng = ChaCha20Rng::seed_from_u64(options.seed.wrapping_add(epoch as u64));
        let mut order = (0..paths.len()).collect::<Vec<_>>();
        order.shuffle(&mut rng);
        let augment = epoch < options.epochs.saturating_sub(options.no_aug_epochs);
        let mut total_loss = 0f64;
        let mut size = options.size;
        for (batch_idx, batch) in order.chunks(options.batch_size).enumerate() {
            if augment && options.multiscale_range > 0 && batch_idx % 10 == 0 {
                let center = options.size / 32;
                let min = center.saturating_sub(options.multiscale_range).max(2);
                size = rng.random_range(min..=center + options.multiscale_range) * 32;
            }
            let mut data = Vec::new();
            let mut truth = Vec::new();
            for &i in batch {
                let (pixels, boxes) = sample(&paths[i], &paths, size, &mut rng, augment)?;
                data.extend(pixels);
                truth.push(boxes);
            }
            let input = Tensor::from_vec(data, (batch.len(), 3, size, size), &net.device)?;
            let prediction = net.forward(&input, true)?;
            let loss = detection_loss_with_l1(&prediction, &truth, size, !augment)?;
            let value = loss.to_scalar::<f32>()?;
            if !value.is_finite() {
                return Err(format!("non-finite loss at epoch {epoch} batch {batch_idx}").into());
            }
            total_loss += value as f64;
            let grads = loss.affine(loss_scale, 0.)?.backward()?;
            // Validate every gradient before changing any master parameter.
            let mut gradients = BTreeMap::new();
            let mut finite = true;
            for (name, var) in &net.variables {
                if name.ends_with("running_mean") || name.ends_with("running_var") {
                    continue;
                }
                if let Some(gradient) = grads.get(var) {
                    let gradient = gradient.affine(1. / loss_scale, 0.)?;
                    if gradient
                        .flatten_all()?
                        .to_vec1::<f32>()?
                        .iter()
                        .any(|v| !v.is_finite())
                    {
                        finite = false;
                        break;
                    }
                    gradients.insert(name.clone(), gradient);
                }
            }
            if !finite {
                if !options.fp16 || loss_scale <= 1. {
                    return Err("non-finite training gradient".into());
                }
                loss_scale = (loss_scale / 2.).max(1.);
                scale_successes = 0;
                step += 1;
                println!("FP16 gradient overflow; skipped update, loss scale {loss_scale}");
                continue;
            }
            let warmup = options.warmup_epochs * batches;
            let total = options.epochs * batches;
            let end = options.no_aug_epochs * batches;
            let progress = step.saturating_sub(warmup) as f64
                / total.saturating_sub(warmup + end).max(1) as f64;
            let rate = if step < warmup {
                options.lr * ((step + 1) as f64 / warmup.max(1) as f64).powi(2)
            } else if step >= total.saturating_sub(end) {
                options.lr * 0.05
            } else {
                options.lr
                    * (0.05 + 0.95 * (1. + (std::f64::consts::PI * progress.min(1.)).cos()) / 2.)
            };
            for (name, var) in &net.variables {
                if name.ends_with("running_mean") || name.ends_with("running_var") {
                    continue;
                }
                if let Some(gradient) = gradients.get(name) {
                    let gradient = if name.ends_with("conv.weight")
                        || name.contains("_preds") && name.ends_with("weight")
                    {
                        (gradient + var.affine(0.0005, 0.)?)?
                    } else {
                        gradient.clone()
                    };
                    let velocity = match momentum.get(name) {
                        Some(previous) => (previous.affine(0.9, 0.)? + &gradient)?,
                        None => gradient.clone(),
                    };
                    let update = (&gradient + velocity.affine(0.9, 0.)?)?;
                    var.set(&(var.as_detached_tensor() - update.affine(rate, 0.)?)?)?;
                    momentum.insert(name.clone(), velocity.detach());
                }
            }
            step += 1;
            ema.update(&net)?;
            if options.fp16 {
                scale_successes += 1;
                if scale_successes >= 2000 {
                    loss_scale = (loss_scale * 2.).min(initial_loss_scale());
                    scale_successes = 0;
                }
            }
            println!(
                "epoch {}/{} batch {}/{} loss {value:.5} lr {rate:.7} size {size}",
                epoch + 1,
                options.epochs,
                batch_idx + 1,
                batches
            );
        }
        let mut ap = None;
        if (epoch + 1) % options.eval_interval == 0 || epoch + 1 == options.epochs {
            ema.apply(&evaluation_net)?;
            let report = crate::evaluation::evaluate_network(
                &evaluation_net,
                &val,
                options.size,
                0.01,
                0.65,
            )?;
            if report.ap50_95 > best {
                best = report.ap50_95;
                save_checkpoint(
                    &net,
                    &ema,
                    &momentum,
                    &options,
                    "best",
                    epoch + 1,
                    step,
                    best,
                    loss_scale,
                    scale_successes,
                )?;
            }
            ap = Some(report.ap50_95);
            println!("validation AP50:95 {:.5}", report.ap50_95);
        }
        save_checkpoint(
            &net,
            &ema,
            &momentum,
            &options,
            "last",
            epoch + 1,
            step,
            best,
            loss_scale,
            scale_successes,
        )?;
        history.push(
            serde_json::json!({"epoch":epoch+1,"loss":total_loss/batches as f64,"ap50_95":ap}),
        );
        atomic_write(
            &options.output.join("history.json"),
            &serde_json::to_vec_pretty(&history)?,
        )?;
    }
    Ok(())
}
#[allow(clippy::too_many_arguments)]
fn save_checkpoint(
    net: &Network,
    ema: &Ema,
    momentum: &BTreeMap<String, Tensor>,
    options: &TrainOptions,
    name: &str,
    epoch: usize,
    step: usize,
    best_ap: f32,
    loss_scale: f64,
    scale_successes: usize,
) -> Result<()> {
    let stage = tempfile::tempdir_in(&options.output)?;
    // Export/evaluation uses EMA; raw master weights are retained for exact resume.
    ema.save(stage.path().join("weights.safetensors"))?;
    net.save(&stage.path().join("training.safetensors"))?;
    let momentum = momentum
        .iter()
        .map(|(name, tensor)| (name.clone(), tensor.clone()))
        .collect::<std::collections::HashMap<_, _>>();
    candle_core::safetensors::save(&momentum, stage.path().join("momentum.safetensors"))?;
    std::fs::write(
        stage.path().join("state.json"),
        serde_json::to_vec_pretty(&State {
            epoch,
            step,
            best_ap,
            options: options.clone(),
            loss_scale,
            scale_successes,
            ema_updates: ema.updates,
        })?,
    )?;
    // Publish a complete immutable checkpoint, then atomically update its pointer.
    let target = options
        .output
        .join(format!("{name}-epoch-{epoch}-step-{step}"));
    if target.exists() {
        return Err(format!("checkpoint already exists: {}", target.display()).into());
    }
    std::fs::rename(stage.path(), &target)?;
    atomic_write(
        &options.output.join(format!("{name}.json")),
        &serde_json::to_vec_pretty(
            &serde_json::json!({"checkpoint":target.file_name().unwrap().to_string_lossy()}),
        )?,
    )?;
    Ok(())
}
fn sample(
    path: &Path,
    paths: &[PathBuf],
    size: usize,
    rng: &mut impl Rng,
    augment: bool,
) -> Result<(Vec<f32>, Vec<BoxLabel>)> {
    let mosaic = augment && rng.random::<f32>() < 0.3;
    let mut image = image::RgbImage::from_pixel(size as u32, size as u32, image::Rgb([114; 3]));
    let mut boxes = Vec::new();
    let samples = if mosaic { 4 } else { 1 };
    for quadrant in 0..samples {
        let path = if quadrant == 0 {
            path
        } else {
            &paths[rng.random_range(0..paths.len())]
        };
        let source = image::open(path)?.to_rgb8();
        let tile = if mosaic { size / 2 } else { size };
        let scale = (tile as f32 / source.width() as f32).min(tile as f32 / source.height() as f32)
            * if augment {
                rng.random_range(0.5f32..1.5)
            } else {
                1.
            };
        let resized = image::imageops::resize(
            &source,
            (source.width() as f32 * scale).max(1.) as u32,
            (source.height() as f32 * scale).max(1.) as u32,
            image::imageops::FilterType::Triangle,
        );
        let origin_x = if mosaic { (quadrant % 2) * tile } else { 0 };
        let origin_y = if mosaic { (quadrant / 2) * tile } else { 0 };
        let shift = if augment {
            (size as f32 * 0.05) as i32
        } else {
            0
        };
        let dx = origin_x as i32
            + if shift > 0 {
                rng.random_range(-shift..=shift)
            } else {
                0
            };
        let dy = origin_y as i32
            + if shift > 0 {
                rng.random_range(-shift..=shift)
            } else {
                0
            };
        for (x, y, p) in resized.enumerate_pixels() {
            let tx = x as i32 + dx;
            let ty = y as i32 + dy;
            if tx >= origin_x as i32
                && ty >= origin_y as i32
                && tx < (origin_x + tile) as i32
                && ty < (origin_y + tile) as i32
            {
                image.put_pixel(tx as u32, ty as u32, *p);
            }
        }
        for b in labels(path)? {
            let [x0, y0, x1, y1] = b.xyxy();
            let x0 = (x0 * scale + dx as f32).clamp(origin_x as f32, (origin_x + tile) as f32);
            let x1 = (x1 * scale + dx as f32).clamp(origin_x as f32, (origin_x + tile) as f32);
            let y0 = (y0 * scale + dy as f32).clamp(origin_y as f32, (origin_y + tile) as f32);
            let y1 = (y1 * scale + dy as f32).clamp(origin_y as f32, (origin_y + tile) as f32);
            if x1 - x0 > 1. && y1 - y0 > 1. {
                boxes.push(BoxLabel {
                    cx: (x0 + x1) / 2.,
                    cy: (y0 + y1) / 2.,
                    w: x1 - x0,
                    h: y1 - y0,
                });
            }
        }
    }
    if augment {
        if rng.random_bool(0.5) {
            image = image::imageops::flip_horizontal(&image);
            for b in &mut boxes {
                b.cx = size as f32 - b.cx;
            }
        }
        let gain = rng.random_range(0.5f32..1.5);
        let saturation = rng.random_range(0.3f32..1.7);
        for p in image.pixels_mut() {
            let mean = (p[0] as f32 + p[1] as f32 + p[2] as f32) / 3.;
            for c in 0..3 {
                p[c] = ((mean + (p[c] as f32 - mean) * saturation) * gain).clamp(0., 255.) as u8;
            }
        }
    }
    let mut data = vec![0f32; size * size * 3];
    for (x, y, p) in image.enumerate_pixels() {
        for c in 0..3 {
            data[c * size * size + y as usize * size + x as usize] = p[2 - c] as f32;
        }
    }
    Ok((data, boxes))
}
pub fn detection_loss(prediction: &Tensor, truth: &[Vec<BoxLabel>], size: usize) -> Result<Tensor> {
    detection_loss_with_l1(prediction, truth, size, false)
}
pub fn detection_loss_with_l1(
    prediction: &Tensor,
    truth: &[Vec<BoxLabel>],
    size: usize,
    use_l1: bool,
) -> Result<Tensor> {
    let (batch, anchors, channels) = prediction.dims3()?;
    if channels != 6 || batch != truth.len() {
        return Err("prediction shape and truth mismatch".into());
    }
    let mut grids = Vec::new();
    for stride in [8, 16, 32] {
        for y in 0..size / stride {
            for x in 0..size / stride {
                grids.push((
                    (x as f32 + 0.5) * stride as f32,
                    (y as f32 + 0.5) * stride as f32,
                    stride as f32,
                ));
            }
        }
    }
    if grids.len() != anchors {
        return Err("prediction anchor count mismatch".into());
    }
    let values = prediction.detach().to_vec3::<f32>()?;
    let mut obj = vec![0f32; batch * anchors];
    let mut assigned = Vec::new();
    let mut assigned_strides = Vec::new();
    let mut targets = Vec::new();
    let mut class_targets = Vec::new();
    for b in 0..batch {
        let mut matches: BTreeMap<usize, (usize, f32, f32)> = BTreeMap::new();
        for (gt_idx, gt) in truth[b].iter().enumerate() {
            let bbox = gt.xyxy();
            let mut costs = Vec::new();
            for (a, &(gx, gy, stride)) in grids.iter().enumerate() {
                let inside = gx > bbox[0] && gx < bbox[2] && gy > bbox[1] && gy < bbox[3];
                let center = (gx - gt.cx).abs() < 2.5 * stride && (gy - gt.cy).abs() < 2.5 * stride;
                if !inside && !center {
                    continue;
                }
                let p = &values[b][a];
                let predicted = BoxLabel {
                    cx: p[0],
                    cy: p[1],
                    w: p[2],
                    h: p[3],
                }
                .xyxy();
                let iou = overlap(bbox, predicted);
                let probability = ((1. / (1. + (-p[4]).exp())) * (1. / (1. + (-p[5]).exp())))
                    .sqrt()
                    .clamp(1e-7, 1. - 1e-7);
                let cost = -probability.ln() - 3. * (iou + 1e-8).ln()
                    + if inside && center { 0. } else { 100000. };
                costs.push((a, cost, iou));
            }
            if costs.is_empty() {
                continue;
            }
            let mut ious = costs.iter().map(|c| c.2).collect::<Vec<_>>();
            ious.sort_by(|a, b| b.total_cmp(a));
            let k = (ious.iter().take(10).sum::<f32>() as usize)
                .max(1)
                .min(costs.len());
            costs.sort_by(|a, b| a.1.total_cmp(&b.1));
            for &(anchor, cost, iou) in costs.iter().take(k) {
                if matches.get(&anchor).is_none_or(|old| cost < old.1) {
                    matches.insert(anchor, (gt_idx, cost, iou));
                }
            }
        }
        for (anchor, (gt, _, iou)) in matches {
            obj[b * anchors + anchor] = 1.;
            assigned.push((b * anchors + anchor) as u32);
            assigned_strides.push(grids[anchor].2);
            targets.extend([
                truth[b][gt].cx,
                truth[b][gt].cy,
                truth[b][gt].w,
                truth[b][gt].h,
            ]);
            class_targets.push(iou);
        }
    }
    let flattened = prediction.reshape((batch * anchors, 6))?;
    let object_logits = flattened.narrow(1, 4, 1)?.flatten_all()?;
    let object_targets = Tensor::from_vec(obj, (batch * anchors,), prediction.device())?;
    let mut loss = bce(&object_logits, &object_targets)?.sum_all()?;
    let count = assigned.len();
    if count > 0 {
        let selected = flattened.index_select(&Tensor::new(assigned, prediction.device())?, 0)?;
        let target = Tensor::from_vec(targets, (count, 4), prediction.device())?;
        let center = selected.narrow(1, 0, 2)?;
        let half = selected.narrow(1, 2, 2)?.affine(0.5, 0.)?;
        let tc = target.narrow(1, 0, 2)?;
        let th = target.narrow(1, 2, 2)?.affine(0.5, 0.)?;
        let lo = (&center - &half)?.maximum(&(&tc - &th)?)?;
        let hi = (&center + &half)?.minimum(&(&tc + &th)?)?;
        let inter = (hi - lo)?.clamp(0f32, f32::MAX)?;
        let area = inter.narrow(1, 0, 1)?.mul(&inter.narrow(1, 1, 1)?)?;
        let pred_area = selected.narrow(1, 2, 1)?.mul(&selected.narrow(1, 3, 1)?)?;
        let true_area = target.narrow(1, 2, 1)?.mul(&target.narrow(1, 3, 1)?)?;
        let iou = area.div(&(pred_area + true_area - area.clone())?.affine(1., 1e-8)?)?;
        let regression = iou.sqr()?.affine(-1., 1.)?.sum_all()?.affine(5., 0.)?;
        let cls_target = Tensor::from_vec(class_targets, (count, 1), prediction.device())?;
        let classification = bce(&selected.narrow(1, 5, 1)?, &cls_target)?.sum_all()?;
        loss = (loss + regression + classification)?;
        if use_l1 {
            // Decode inversion preserves YOLOX's final-epoch raw-box L1 loss.
            // The grid cancels for centers; width/height are logarithmic targets.
            let strides = Tensor::from_vec(assigned_strides, (count, 1), prediction.device())?;
            let centers = selected
                .narrow(1, 0, 2)?
                .sub(&target.narrow(1, 0, 2)?)?
                .broadcast_div(&strides)?
                .abs()?
                .sum_all()?;
            let predicted_wh = selected
                .narrow(1, 2, 2)?
                .clamp(1e-30f32, f32::MAX)?
                .broadcast_div(&strides)?
                .log()?;
            let target_wh = target
                .narrow(1, 2, 2)?
                .broadcast_div(&strides)?
                .affine(1., 1e-8)?
                .log()?;
            let sizes = (predicted_wh - target_wh)?.abs()?.sum_all()?;
            loss = (loss + centers + sizes)?;
        }
    }
    Ok(loss.affine(1. / count.max(1) as f64, 0.)?)
}
fn bce(logits: &Tensor, target: &Tensor) -> candle_core::Result<Tensor> {
    logits
        .clamp(0f32, f32::MAX)?
        .sub(&logits.mul(target)?)?
        .add(&logits.abs()?.neg()?.exp()?.affine(1., 1.)?.log()?)
}
