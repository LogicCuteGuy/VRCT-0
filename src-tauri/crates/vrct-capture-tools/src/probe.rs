use crate::{console, run_id, source::Eye};
use clap::Parser;
use image::RgbImage;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    path::PathBuf,
    sync::atomic::Ordering,
    time::{Duration, Instant},
};
#[derive(Parser, Debug)]
#[command(
    about = "Standalone OpenVR D3D11 diagnostic probe; saves local images/report, never labels or uploads."
)]
pub struct Args {
    #[arg(long, default_value_t = 5)]
    pub frames: usize,
    #[arg(long, default_value_t = 1.0)]
    pub interval: f64,
    #[arg(long,value_enum,default_value_t=Eye::Left)]
    pub eye: Eye,
    #[arg(long, default_value = "tmp/openvr_probe")]
    pub out: PathBuf,
}
impl Args {
    pub fn validate(&self) -> Result<(), String> {
        if !(1..=120).contains(&self.frames)
            || !self.interval.is_finite()
            || !(0.1..=10.0).contains(&self.interval)
        {
            return Err("Use 1..120 frames and an interval of 0.1..10 seconds".into());
        }
        Ok(())
    }
}
pub fn image_stats(frame: &RgbImage, previous: Option<&RgbImage>) -> Value {
    let count = frame.as_raw().len() as f64;
    let mean = if count > 0.0 {
        frame.as_raw().iter().map(|v| *v as f64).sum::<f64>() / count
    } else {
        0.0
    };
    let variance = if count > 0.0 {
        frame
            .as_raw()
            .iter()
            .map(|v| (*v as f64 - mean).powi(2))
            .sum::<f64>()
            / count
    } else {
        0.0
    };
    let changed = previous
        .filter(|previous| previous.dimensions() == frame.dimensions())
        .map(|previous| {
            let pixels = frame.width() as f64 * frame.height() as f64;
            if pixels > 0.0 {
                frame
                    .pixels()
                    .zip(previous.pixels())
                    .filter(|(a, b)| a != b)
                    .count() as f64
                    / pixels
            } else {
                0.0
            }
        });
    json!({"sha256":hex::encode(Sha256::digest(frame.as_raw())),"mean":mean,"std":variance.sqrt(),"changed_pixel_fraction":changed})
}
pub fn run(args: Args) -> Result<(), String> {
    args.validate()?;
    if !cfg!(all(windows, target_arch = "x86_64")) {
        return Err("This probe requires Windows x64".into());
    }
    let stop = console::interrupt_flag()?;
    let stamp = run_id()?;
    let output = args.out.join(&stamp);
    fs::create_dir_all(&output).map_err(|e| e.to_string())?;
    let mut report = json!({"utc_start":stamp,"eye":args.eye.name(),"status":"ERROR","packages":{"vrct_capture_tools":env!("CARGO_PKG_VERSION"),"openvr_sdk":"2.15.6","image":"0.25"},"frames":[]});
    let result = (|| -> Result<(), String> {
        #[cfg(all(windows, target_arch = "x86_64"))]
        {
            let mut mirror =
                vrct_core::openvr::capture::MirrorCapture::new_eye(args.eye == Eye::Right)?;
            let diagnostics =
                serde_json::to_value(mirror.diagnostics()).map_err(|e| e.to_string())?;
            for key in [
                "hmd",
                "recommended_size",
                "adapter_index",
                "view_format",
                "texture",
            ] {
                report[key] = diagnostics[key].clone();
            }
            let mut previous = None;
            for index in 0..args.frames {
                if index > 0 {
                    let start = Instant::now();
                    while start.elapsed() < Duration::from_secs_f64(args.interval)
                        && !stop.load(Ordering::Acquire)
                    {
                        std::thread::sleep(Duration::from_millis(20));
                    }
                }
                if stop.load(Ordering::Acquire) {
                    return Err("Probe interrupted".into());
                }
                let start = Instant::now();
                let frame = mirror.read_mirror()?;
                let capture_ms = start.elapsed().as_secs_f64() * 1000.0;
                let processes = mirror.scene_processes();
                let state = mirror.scene_state();
                let (renderer_pid, focus_pid, frame_index) = state
                    .map(|state| (state.renderer_pid, state.focus_pid, Some(state.frame_index)))
                    .unwrap_or((processes.0, processes.1, None));
                let renderer_name = vrct_core::ocr::hwnd::process_name(renderer_pid);
                let file = format!("{}_{index:03}.png", args.eye.name());
                let mut record = image_stats(&frame, previous.as_ref());
                record.as_object_mut().unwrap().extend(json!({"index":index,"utc":chrono::Utc::now().to_rfc3339(),"capture_ms":(capture_ms*1000.0).round()/1000.0,"focus_pid":focus_pid,"renderer_pid":renderer_pid,"renderer_name":renderer_name,"compositor_frame_index":frame_index,"vrchat_windows":vrct_core::ocr::hwnd::vrchat_windows(),"file":file}).as_object().unwrap().clone());
                let mut png = File::options()
                    .write(true)
                    .create_new(true)
                    .open(output.join(&file))
                    .map_err(|e| e.to_string())?;
                use image::ImageEncoder;
                image::codecs::png::PngEncoder::new_with_quality(
                    &mut png,
                    image::codecs::png::CompressionType::Fast,
                    image::codecs::png::FilterType::Adaptive,
                )
                .write_image(
                    frame.as_raw(),
                    frame.width(),
                    frame.height(),
                    image::ExtendedColorType::Rgb8,
                )
                .map_err(|e| e.to_string())?;
                report["frames"]
                    .as_array_mut()
                    .unwrap()
                    .push(record.clone());
                println!("{record}");
                previous = Some(frame);
            }
            report["row_pitch"] = json!(mirror.diagnostics().row_pitch);
            report["status"] = json!("CAPTURED");
            // Mirror's lease drops only after D3D11 objects and before report publication.
            drop(mirror);
            Ok(())
        }
        #[cfg(not(all(windows, target_arch = "x86_64")))]
        Err("This probe requires Windows x64".into())
    })();
    if let Err(error) = &result {
        report["error"] = json!(error);
        eprintln!("{error}");
    }
    let file = File::options()
        .write(true)
        .create_new(true)
        .open(output.join("report.json"))
        .map_err(|e| e.to_string())?;
    serde_json::to_writer_pretty(file, &report).map_err(|e| e.to_string())?;
    println!("Report: {}", output.join("report.json").display());
    result
}
