//! PP-OCR pre/postprocessing follows RapidAI/RapidOCR (Apache-2.0):
//! https://github.com/RapidAI/RapidOCR/tree/f65c7da00e72c19c258245e8e0e5f33af14488be/python/rapidocr.
//! BGR tensors, DB polygons/unclip, 180-degree classifier and CTC labels from
//! ONNX metadata use the same model files as the previous OCR implementation.
use super::{merge_lines, model_spec, models, Config, Line, Scanner};
use crate::audio::silero::OnnxRuntime;
use image::imageops::{self, FilterType};
use image::{GrayImage, Luma, RgbImage};
use imageproc::contours::{find_contours, BorderType};
use imageproc::geometric_transformations::{warp_into, Interpolation, Projection};
use ort::{session::Session, value::Tensor};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

fn session(path: &std::path::Path) -> Result<Session, String> {
    let builder = Session::builder().map_err(|e| e.to_string())?;
    let builder = builder.with_intra_threads(1).map_err(|e| e.to_string())?;
    let mut builder = builder.with_inter_threads(1).map_err(|e| e.to_string())?;
    builder
        .commit_from_file(path)
        .map_err(|e| format!("OCR model {}: {e}", path.display()))
}
fn infer(
    session: &mut Session,
    shape: [usize; 4],
    values: Vec<f32>,
) -> Result<(Vec<i64>, Vec<f32>), String> {
    let input = Tensor::from_array((shape, values)).map_err(|e| e.to_string())?;
    let output = session
        .run(ort::inputs![input])
        .map_err(|e| e.to_string())?;
    let (shape, values) = output[0]
        .try_extract_tensor::<f32>()
        .map_err(|e| e.to_string())?;
    Ok((shape.to_vec(), values.to_vec()))
}
fn tensor(image: &RgbImage, width: u32, height: u32, normalise: bool, padding: f32) -> Vec<f32> {
    let mut out = vec![padding; width as usize * height as usize * 3];
    for (x, y, pixel) in image.enumerate_pixels() {
        if x >= width || y >= height {
            continue;
        }
        for channel in 0..3 {
            let value = pixel[2 - channel] as f32;
            out[channel * width as usize * height as usize
                + y as usize * width as usize
                + x as usize] = if normalise {
                value / 127.5 - 1.0
            } else {
                value
            };
        }
    }
    out
}

#[derive(Clone, Copy, Debug)]
pub struct Bubble {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub score: f32,
}
pub struct BubbleDetector {
    session: Session,
}
impl BubbleDetector {
    pub fn new(config: &Config) -> Result<Self, String> {
        // This application-specific detector has a separate restrictive license.
        // It must be supplied by an authorized user, never embedded in a fork.
        let mut candidates = Vec::new();
        if let Some(path) = std::env::var_os("VRCT_OCR_BUBBLE_MODEL") {
            candidates.push(std::path::PathBuf::from(path));
        }
        candidates.push(config.local.join("resources/ocr/chatbox_yolox_tiny.onnx"));
        candidates.push(config.local.join("weights/ocr/chatbox_yolox_tiny.onnx"));
        #[cfg(debug_assertions)]
        candidates.push(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../../src-python/models/ocr/onnx/chatbox_yolox_tiny.onnx"),
        );
        let path = candidates.into_iter().find(|p| p.is_file()).ok_or(
            "OCR_DISABLED_MODEL_MISSING: provide an authorized chatbox_yolox_tiny.onnx via VRCT_OCR_BUBBLE_MODEL",
        )?;
        Ok(Self {
            session: session(&path)?,
        })
    }
    pub fn detect(&mut self, frame: &RgbImage) -> Result<Vec<Bubble>, String> {
        if frame.width() == 0 || frame.height() == 0 {
            return Ok(Vec::new());
        }
        let scale = (1280.0 / frame.width() as f32).min(1280.0 / frame.height() as f32);
        let width = (frame.width() as f32 * scale) as u32;
        let height = (frame.height() as f32 * scale) as u32;
        let width = width.max(1);
        let height = height.max(1);
        let canvas_width = width.div_ceil(32) * 32;
        let canvas_height = height.div_ceil(32) * 32;
        let resized = imageops::resize(frame, width, height, FilterType::Triangle);
        let (_, predictions) = infer(
            &mut self.session,
            [1, 3, canvas_height as usize, canvas_width as usize],
            tensor(&resized, canvas_width, canvas_height, false, 114.0),
        )?;
        if !predictions.len().is_multiple_of(6) {
            return Err("YOLOX output shape is invalid".into());
        }
        let mut boxes = Vec::new();
        for row in predictions.chunks_exact(6) {
            let score = row[4] * row[5];
            if !score.is_finite() || score < 0.7 || row[..4].iter().any(|v| !v.is_finite()) {
                continue;
            }
            boxes.push((
                [
                    row[0] - row[2] * 0.5,
                    row[1] - row[3] * 0.5,
                    row[0] + row[2] * 0.5,
                    row[1] + row[3] * 0.5,
                ],
                score,
            ));
        }
        boxes.sort_by(|a, b| b.1.total_cmp(&a.1));
        let mut kept: Vec<([f32; 4], f32)> = Vec::new();
        let mut bubbles = Vec::new();
        for (coords, score) in boxes {
            if kept.iter().any(|(other, _)| {
                let intersection = (coords[2].min(other[2]) - coords[0].max(other[0])).max(0.0)
                    * (coords[3].min(other[3]) - coords[1].max(other[1])).max(0.0);
                let union = (coords[2] - coords[0]) * (coords[3] - coords[1])
                    + (other[2] - other[0]) * (other[3] - other[1])
                    - intersection;
                intersection / union.max(1e-6) > 0.65
            }) {
                continue;
            }
            kept.push((coords, score));
            let x0 = (coords[0] / scale).round_ties_even() as i64 - 4;
            let y0 = (coords[1] / scale).round_ties_even() as i64 - 4;
            let x1 = (coords[2] / scale).round_ties_even() as i64 + 4;
            let y1 = (coords[3] / scale).round_ties_even() as i64 + 4;
            let x0 = x0.clamp(0, frame.width() as i64) as u32;
            let y0 = y0.clamp(0, frame.height() as i64) as u32;
            let x1 = x1.clamp(0, frame.width() as i64) as u32;
            let y1 = y1.clamp(0, frame.height() as i64) as u32;
            if x1 > x0 + 1 && y1 > y0 + 1 {
                bubbles.push(Bubble {
                    x: x0,
                    y: y0,
                    width: x1 - x0,
                    height: y1 - y0,
                    score,
                });
            }
        }
        Ok(bubbles)
    }
}
pub fn non_max_suppression(mut boxes: Vec<Bubble>, threshold: f32) -> Vec<Bubble> {
    boxes.sort_by(|a, b| b.score.total_cmp(&a.score));
    let mut kept: Vec<Bubble> = Vec::new();
    for candidate in boxes {
        if kept.iter().any(|old| {
            let x0 = old.x.max(candidate.x);
            let y0 = old.y.max(candidate.y);
            let x1 = (old.x + old.width).min(candidate.x + candidate.width);
            let y1 = (old.y + old.height).min(candidate.y + candidate.height);
            let intersection = x1.saturating_sub(x0) as f32 * y1.saturating_sub(y0) as f32;
            let union = old.width as f32 * old.height as f32
                + candidate.width as f32 * candidate.height as f32
                - intersection;
            intersection / union.max(1e-6) > threshold
        }) {
            continue;
        }
        kept.push(candidate);
    }
    kept
}

pub struct PaddleReader {
    detector: Session,
    recognizer: Session,
    classifier: Session,
    labels: Vec<String>,
    arabic: bool,
}
impl PaddleReader {
    pub fn new(config: &Config, cancelled: Arc<AtomicBool>) -> Result<Self, String> {
        let (det, rec) = model_spec(&config.language).ok_or("OCR_DISABLED_UNSUPPORTED_LANGUAGE")?;
        let detector = session(&models::resolve(config, det, cancelled.clone())?)?;
        let recognizer = session(&models::resolve(config, rec, cancelled.clone())?)?;
        let classifier = session(&models::resolve(
            config,
            "ch_ppocr_mobile_v2.0_cls_mobile",
            cancelled,
        )?)?;
        let dictionary = recognizer
            .metadata()
            .map_err(|e| e.to_string())?
            .custom("character")
            .ok_or("OCR recognizer has no embedded character dictionary")?;
        let mut labels = vec![String::new()];
        labels.extend(
            dictionary
                .lines()
                .map(|line| line.trim_end_matches('\r').to_owned()),
        );
        labels.push(" ".into());
        Ok(Self {
            detector,
            recognizer,
            classifier,
            labels,
            arabic: config.language == "Arabic",
        })
    }
    pub fn recognize(&mut self, crop: &RgbImage, confidence: f32) -> Result<Vec<Line>, String> {
        if crop.width() == 0 || crop.height() == 0 {
            return Ok(Vec::new());
        }
        let prepared = prepare_crop(crop);
        let crop = &prepared;
        let ratio = (320.0 / crop.width().min(crop.height()) as f32).max(1.0);
        let width = ((crop.width() as f32 * ratio / 32.0).round_ties_even() as u32 * 32).max(32);
        let height = ((crop.height() as f32 * ratio / 32.0).round_ties_even() as u32 * 32).max(32);
        if width > 8192 || height > 8192 {
            return Err("OCR crop aspect ratio exceeds inference limit".into());
        }
        let resized = imageops::resize(crop, width, height, FilterType::Triangle);
        let (shape, probability) = infer(
            &mut self.detector,
            [1, 3, height as usize, width as usize],
            tensor(&resized, width, height, true, 0.0),
        )?;
        if shape.len() != 4 || shape[0] != 1 || shape[1] != 1 || shape[2] <= 0 || shape[3] <= 0 {
            return Err("OCR DB output shape is invalid".into());
        }
        let output_width = shape[3] as u32;
        let output_height = shape[2] as u32;
        if probability.len() != output_width as usize * output_height as usize {
            return Err("OCR DB tensor length is invalid".into());
        }
        // RapidOCR applies a 2x2 dilation with OpenCV's anchor (1, 1).
        let bitmap = GrayImage::from_fn(output_width, output_height, |x, y| {
            Luma([
                if (y.saturating_sub(1)..=y).any(|sy| {
                    (x.saturating_sub(1)..=x).any(|sx| {
                        probability[sy as usize * output_width as usize + sx as usize] > 0.3
                    })
                }) {
                    255
                } else {
                    0
                },
            ])
        });
        let mut boxes = Vec::new();
        for contour in find_contours::<i32>(&bitmap)
            .into_iter()
            .filter(|contour| contour.border_type == BorderType::Outer)
            .take(1000)
        {
            if contour.points.len() < 4 {
                continue;
            }
            let points = imageproc::geometry::min_area_rect(&contour.points);
            let points = points.map(|point| [point.x as f32, point.y as f32]);
            let width = distance(points[0], points[1]);
            let height = distance(points[0], points[3]);
            if width.min(height) < 3.0 {
                continue;
            }
            let min_x = points
                .iter()
                .map(|p| p[0])
                .fold(f32::INFINITY, f32::min)
                .floor()
                .max(0.0) as u32;
            let min_y = points
                .iter()
                .map(|p| p[1])
                .fold(f32::INFINITY, f32::min)
                .floor()
                .max(0.0) as u32;
            let max_x = points
                .iter()
                .map(|p| p[0])
                .fold(0.0, f32::max)
                .ceil()
                .min(output_width as f32 - 1.0) as u32;
            let max_y = points
                .iter()
                .map(|p| p[1])
                .fold(0.0, f32::max)
                .ceil()
                .min(output_height as f32 - 1.0) as u32;
            let mut sum = 0.0;
            let mut count = 0usize;
            for y in min_y..=max_y {
                for x in min_x..=max_x {
                    if inside([x as f32, y as f32], &points) {
                        sum += probability[y as usize * output_width as usize + x as usize];
                        count += 1;
                    }
                }
            }
            if count == 0 || sum / (count as f32) < 0.5 {
                continue;
            }
            // DBPostProcess offsets the minimum rectangle by area*1.6/perimeter.
            // Offsetting its four supporting lines gives the same expanded
            // minimum rectangle as clipping the rounded offset polygon.
            let grow = 0.8 * width * height / (width + height);
            let horizontal = [
                (points[1][0] - points[0][0]) / width,
                (points[1][1] - points[0][1]) / width,
            ];
            let vertical = [
                (points[3][0] - points[0][0]) / height,
                (points[3][1] - points[0][1]) / height,
            ];
            let signs = [[-1.0, -1.0], [1.0, -1.0], [1.0, 1.0], [-1.0, 1.0]];
            let mut expanded = points;
            for index in 0..4 {
                for axis in 0..2 {
                    expanded[index][axis] = ((points[index][axis]
                        + grow
                            * (signs[index][0] * horizontal[axis]
                                + signs[index][1] * vertical[axis]))
                        * (if axis == 0 {
                            crop.width() as f32 / output_width as f32
                        } else {
                            crop.height() as f32 / output_height as f32
                        }))
                    .round_ties_even()
                    .clamp(
                        0.0,
                        if axis == 0 {
                            crop.width() as f32
                        } else {
                            crop.height() as f32
                        },
                    );
                }
            }
            boxes.push(expanded);
        }
        boxes.sort_by(|a, b| {
            a[0][1]
                .total_cmp(&b[0][1])
                .then(a[0][0].total_cmp(&b[0][0]))
        });
        let mut lines = Vec::new();
        for points in boxes {
            let Some(mut line) = perspective_crop(crop, points) else {
                continue;
            };
            let cls_image = resize_line(&line, 48, 192);
            let (_, classes) = infer(
                &mut self.classifier,
                [1, 3, 48, 192],
                tensor(&cls_image, 192, 48, true, 0.0),
            )?;
            if classes.get(1).copied().unwrap_or(0.0) > 0.9
                && classes[1] > classes.first().copied().unwrap_or(0.0)
            {
                line = imageops::rotate180(&line);
            }
            let width = (48.0 * (line.width() as f32 / line.height() as f32))
                .ceil()
                .max(320.0) as u32;
            if width > 8192 {
                continue;
            }
            let normalized = resize_line(&line, 48, width);
            let (shape, prediction) = infer(
                &mut self.recognizer,
                [1, 3, 48, width as usize],
                tensor(&normalized, width, 48, true, 0.0),
            )?;
            if shape.len() != 3 || shape[0] != 1 || shape[1] <= 0 || shape[2] <= 0 {
                return Err("OCR CTC output shape is invalid".into());
            }
            let (mut text, score) = decode_ctc(&prediction, shape[2] as usize, &self.labels)?;
            if self.arabic {
                let bidi = unicode_bidi::BidiInfo::new(&text, None);
                text = bidi
                    .paragraphs
                    .iter()
                    .map(|para| bidi.reorder_line(para, para.range.clone()))
                    .collect::<Vec<_>>()
                    .join("");
            }
            if score >= confidence && !text.trim().is_empty() {
                lines.push(Line {
                    text: text.trim().into(),
                    confidence: score,
                    top: points[0][1],
                });
            }
        }
        Ok(lines)
    }
}
pub fn decode_ctc(
    prediction: &[f32],
    classes: usize,
    labels: &[String],
) -> Result<(String, f32), String> {
    if classes == 0 || !prediction.len().is_multiple_of(classes) || classes != labels.len() {
        return Err("OCR dictionary does not match CTC output".into());
    }
    let mut last = usize::MAX;
    let mut text = String::new();
    let mut confidence = 0.0;
    let mut count = 0;
    for row in prediction.chunks_exact(classes) {
        let Some((index, &score)) = row.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)) else {
            continue;
        };
        if index != 0 && index != last && score.is_finite() {
            text.push_str(&labels[index]);
            confidence += score;
            count += 1;
        }
        last = index;
    }
    Ok((
        text,
        if count == 0 {
            0.0
        } else {
            confidence / count as f32
        },
    ))
}
fn resize_line(line: &RgbImage, height: u32, width: u32) -> RgbImage {
    imageops::resize(
        line,
        ((height as f32 * line.width() as f32 / line.height().max(1) as f32).ceil() as u32)
            .min(width)
            .max(1),
        height,
        FilterType::Triangle,
    )
}
fn prepare_crop(crop: &RgbImage) -> RgbImage {
    let mut image = crop.clone();
    // The global RapidOCR resize happens before DB's own inference resize.
    for (limit, expand) in [(2000.0, false), (30.0, true)] {
        let side = if expand {
            image.width().min(image.height())
        } else {
            image.width().max(image.height())
        } as f32;
        if (expand && side < limit) || (!expand && side > limit) {
            let ratio = limit / side;
            let width =
                ((image.width() as f32 * ratio).floor() / 32.0).round_ties_even() as u32 * 32;
            let height =
                ((image.height() as f32 * ratio).floor() / 32.0).round_ties_even() as u32 * 32;
            image = imageops::resize(&image, width.max(32), height.max(32), FilterType::Triangle);
        }
    }
    if image.height() <= 30 || image.width() as f32 / image.height() as f32 > 8.0 {
        let new_height = (image.width() / 8).max(30) * 2;
        let padding = new_height.abs_diff(image.height()) / 2;
        let mut padded = RgbImage::new(image.width(), image.height() + padding * 2);
        imageops::replace(&mut padded, &image, 0, padding as i64);
        image = padded;
    }
    image
}
fn distance(a: [f32; 2], b: [f32; 2]) -> f32 {
    (a[0] - b[0]).hypot(a[1] - b[1])
}
fn inside(point: [f32; 2], polygon: &[[f32; 2]; 4]) -> bool {
    let mut sign = 0.0f32;
    for i in 0..4 {
        let a = polygon[i];
        let b = polygon[(i + 1) % 4];
        let cross = (b[0] - a[0]) * (point[1] - a[1]) - (b[1] - a[1]) * (point[0] - a[0]);
        if cross != 0.0 {
            if sign != 0.0 && cross.signum() != sign {
                return false;
            }
            sign = cross.signum();
        }
    }
    true
}
fn perspective_crop(image: &RgbImage, points: [[f32; 2]; 4]) -> Option<RgbImage> {
    let width = distance(points[0], points[1]).max(distance(points[2], points[3])) as u32;
    let height = distance(points[0], points[3]).max(distance(points[1], points[2])) as u32;
    if width < 2 || height < 2 || width > 8192 || height > 8192 {
        return None;
    }
    let target = [
        [0.0, 0.0],
        [width as f32, 0.0],
        [width as f32, height as f32],
        [0.0, height as f32],
    ];
    // Extend edge pixels so the bicubic sampler has OpenCV's BORDER_REPLICATE
    // behavior even when a detected box touches the image boundary.
    let bordered = RgbImage::from_fn(image.width() + 4, image.height() + 4, |x, y| {
        *image.get_pixel(
            x.saturating_sub(2).min(image.width() - 1),
            y.saturating_sub(2).min(image.height() - 1),
        )
    });
    let projection = Projection::from_control_points(
        points.map(|p| (p[0] + 2.0, p[1] + 2.0)),
        target.map(|p| (p[0], p[1])),
    )?;
    let mut output = RgbImage::new(width, height);
    warp_into(
        &bordered,
        &projection,
        Interpolation::Bicubic,
        image::Rgb([0, 0, 0]),
        &mut output,
    );
    if height as f32 / width as f32 >= 1.5 {
        Some(imageops::rotate270(&output))
    } else {
        Some(output)
    }
}

pub struct NativeScanner {
    reader: PaddleReader,
    bubbles: BubbleDetector,
    language: String,
    cancelled: Arc<AtomicBool>,
    #[cfg(windows)]
    hwnd: super::hwnd::HwndCapture,
    #[cfg(all(windows, target_arch = "x86_64"))]
    mirror: Option<crate::openvr::capture::MirrorCapture>,
    #[cfg(all(windows, target_arch = "x86_64"))]
    last_mirror_attempt: Option<Instant>,
}
impl NativeScanner {
    pub fn new(config: &Config, cancelled: Arc<AtomicBool>) -> Result<Self, String> {
        let library = OnnxRuntime::locate()
            .ok_or("OCR_DISABLED_ENGINE_UNAVAILABLE: ONNX Runtime library is missing")?;
        OnnxRuntime::load(&library)?;
        let bubbles = BubbleDetector::new(config)?;
        let reader = PaddleReader::new(config, cancelled.clone())?;
        Ok(Self {
            reader,
            bubbles,
            language: config.language.clone(),
            cancelled,
            #[cfg(windows)]
            hwnd: super::hwnd::HwndCapture::new(config.window_title.clone()),
            #[cfg(all(windows, target_arch = "x86_64"))]
            mirror: None,
            #[cfg(all(windows, target_arch = "x86_64"))]
            last_mirror_attempt: None,
        })
    }
    pub fn recognize_frame(
        &mut self,
        frame: &RgbImage,
        config: &Config,
    ) -> Result<Vec<String>, String> {
        let deadline = Instant::now() + config.interval.mul_f32(0.8);
        let mut texts = Vec::new();
        for bubble in self.bubbles.detect(frame)?.into_iter().take(6) {
            if self.cancelled.load(Ordering::Acquire) || Instant::now() > deadline {
                break;
            }
            let crop = imageops::crop_imm(frame, bubble.x, bubble.y, bubble.width, bubble.height)
                .to_image();
            let text = merge_lines(self.reader.recognize(&crop, config.confidence)?);
            if !text.is_empty() {
                texts.push(text);
            }
        }
        Ok(texts)
    }
}
impl Scanner for NativeScanner {
    fn configure(&mut self, config: &Config) -> Result<(), String> {
        if self.language != config.language {
            self.reader = PaddleReader::new(config, self.cancelled.clone())?;
            self.language = config.language.clone();
        }
        #[cfg(windows)]
        self.hwnd.set_title(&config.window_title);
        Ok(())
    }
    fn scan(&mut self, config: &Config, cancelled: &AtomicBool) -> Result<Vec<String>, String> {
        if cancelled.load(Ordering::Acquire) {
            return Ok(Vec::new());
        }
        #[cfg(windows)]
        {
            let mut frame = None;
            #[cfg(target_arch = "x86_64")]
            {
                if self.mirror.is_none()
                    && self
                        .last_mirror_attempt
                        .is_none_or(|last| last.elapsed() >= std::time::Duration::from_secs(5))
                {
                    self.last_mirror_attempt = Some(Instant::now());
                    self.mirror = crate::openvr::capture::MirrorCapture::new().ok();
                }
                if let Some(mirror) = self.mirror.as_mut() {
                    match mirror.capture() {
                        Ok(value) => frame = value,
                        Err(_) => self.mirror = None,
                    }
                }
            }
            if frame.is_none() {
                frame = self.hwnd.capture()?;
            }
            if let Some(frame) = frame {
                return self.recognize_frame(&frame, config);
            }
            Ok(Vec::new())
        }
        #[cfg(not(windows))]
        {
            let _ = config;
            Err("Native window OCR is only available on Windows".into())
        }
    }
}
