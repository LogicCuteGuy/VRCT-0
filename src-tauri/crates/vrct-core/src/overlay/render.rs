//! Pure Rust text shaping and rasterization for the two VRCT overlay layouts.
use crate::pipeline::host::{LargeLog, SmallLog};
use fontdue::{Font, FontSettings};
use serde_json::Value;
use std::{collections::VecDeque, path::Path};

#[derive(Clone, Debug)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}
impl Frame {
    pub fn transparent() -> Self {
        Self {
            width: 1,
            height: 1,
            pixels: vec![0; 4],
        }
    }
    fn blank(width: usize, height: usize) -> Result<Self, String> {
        let length = width
            .checked_mul(height)
            .and_then(|n| n.checked_mul(4))
            .filter(|&n| n <= 128 * 1024 * 1024)
            .ok_or("Overlay image exceeds 128 MiB limit")?;
        let mut pixels = Vec::new();
        pixels
            .try_reserve_exact(length)
            .map_err(|e| e.to_string())?;
        pixels.resize(length, 0);
        Ok(Self {
            width: width as u32,
            height: height as u32,
            pixels,
        })
    }
    fn blend(&mut self, x: i32, y: i32, color: [u8; 3], alpha: u8) {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return;
        }
        let offset = (y as usize * self.width as usize + x as usize) * 4;
        let a = alpha as f32 / 255.;
        let old = self.pixels[offset + 3] as f32 / 255.;
        let out = a + old * (1. - a);
        if out == 0. {
            return;
        }
        for (i, c) in color.iter().enumerate() {
            self.pixels[offset + i] =
                (((*c as f32 * a) + (self.pixels[offset + i] as f32 * old * (1. - a))) / out)
                    .round() as u8;
        }
        self.pixels[offset + 3] = (out * 255.).round() as u8;
    }
    fn paste(&mut self, frame: &Frame, x: i32, y: i32) {
        for yy in 0..frame.height {
            for xx in 0..frame.width {
                let offset = (yy as usize * frame.width as usize + xx as usize) * 4;
                self.blend(
                    x + xx as i32,
                    y + yy as i32,
                    [
                        frame.pixels[offset],
                        frame.pixels[offset + 1],
                        frame.pixels[offset + 2],
                    ],
                    frame.pixels[offset + 3],
                );
            }
        }
    }
    fn background(&mut self, radius: i32) {
        let w = self.width as i32;
        let h = self.height as i32;
        let radius = radius.min(w / 2).min(h / 2);
        for y in 0..h {
            for x in 0..w {
                let cx = x.clamp(radius, w - 1 - radius);
                let cy = y.clamp(radius, h - 1 - radius);
                if (x - cx).pow(2) + (y - cy).pow(2) <= radius.pow(2) {
                    self.blend(x, y, [41, 42, 45], 255);
                }
            }
        }
    }
}

struct Typeface {
    bytes: Vec<u8>,
    raster: Font,
}
impl Typeface {
    fn load(path: &Path) -> Result<Self, String> {
        let bytes =
            std::fs::read(path).map_err(|e| format!("Overlay font {}: {e}", path.display()))?;
        let raster =
            Font::from_bytes(bytes.clone(), FontSettings::default()).map_err(str::to_string)?;
        if rustybuzz::Face::from_slice(&bytes, 0).is_none() {
            return Err(format!("Invalid font {}", path.display()));
        }
        Ok(Self { bytes, raster })
    }
    fn shape(&self, text: &str, size: f32) -> (rustybuzz::GlyphBuffer, f32) {
        let face = rustybuzz::Face::from_slice(&self.bytes, 0).expect("validated font");
        let scale = size / face.units_per_em() as f32;
        let mut buffer = rustybuzz::UnicodeBuffer::new();
        buffer.push_str(text);
        buffer.guess_segment_properties();
        (rustybuzz::shape(&face, &[], buffer), scale)
    }
    fn width(&self, text: &str, size: f32) -> f32 {
        let (shaped, scale) = self.shape(text, size);
        shaped
            .glyph_positions()
            .iter()
            .map(|p| p.x_advance as f32 * scale)
            .sum::<f32>()
            .abs()
    }
    fn draw(&self, frame: &mut Frame, text: &str, size: f32, x: f32, y: f32, color: [u8; 3]) {
        let (shaped, scale) = self.shape(text, size);
        let baseline = self
            .raster
            .horizontal_line_metrics(size)
            .map_or(size, |m| size / 2. + (m.ascent + m.descent) / 2.);
        let mut cursor = x;
        for (info, pos) in shaped.glyph_infos().iter().zip(shaped.glyph_positions()) {
            let (metrics, bitmap) = self.raster.rasterize_indexed(info.glyph_id as u16, size);
            let left = (cursor + pos.x_offset as f32 * scale).round() as i32 + metrics.xmin;
            let top = (y + baseline - pos.y_offset as f32 * scale).round() as i32
                - metrics.ymin
                - metrics.height as i32;
            for yy in 0..metrics.height {
                for xx in 0..metrics.width {
                    frame.blend(
                        left + xx as i32,
                        top + yy as i32,
                        color,
                        bitmap[yy * metrics.width + xx],
                    );
                }
            }
            cursor += pos.x_advance as f32 * scale;
        }
    }
}

#[derive(Clone)]
struct Record {
    direction: String,
    message: Option<String>,
    language: Option<String>,
    translation: Vec<String>,
    languages: Value,
    ruby: Vec<Value>,
    translation_ruby: Vec<Vec<Value>>,
    time: String,
}
pub struct Renderer {
    fonts: Vec<Typeface>,
    history: VecDeque<Record>,
}
enum Align {
    Center,
    Left,
    Right,
}
impl Renderer {
    pub fn load(fonts: impl AsRef<Path>) -> Result<Self, String> {
        let fonts = fonts.as_ref();
        let fonts = [
            "NotoSansJP-Regular.ttf",
            "NotoSansKR-Regular.ttf",
            "NotoSansSC-Regular.ttf",
            "NotoSansTC-Regular.ttf",
        ]
        .iter()
        .map(|name| Typeface::load(&fonts.join(name)))
        .collect::<Result<_, _>>()?;
        Ok(Self {
            fonts,
            history: VecDeque::new(),
        })
    }
    fn font(&self, language: Option<&str>) -> &Typeface {
        &self.fonts[match language {
            Some("Korean") => 1,
            Some("Chinese Simplified") => 2,
            Some("Chinese Traditional") => 3,
            _ => 0,
        }]
    }
    // Keep independent text, ruby and layout parameters explicit at render call sites.
    #[allow(clippy::too_many_arguments)]
    fn textbox(
        &self,
        text: &str,
        language: Option<&str>,
        ruby: &[Value],
        width: usize,
        size: usize,
        small: bool,
        align: Align,
    ) -> Result<Frame, String> {
        if text.chars().count() > 32_768 {
            return Err("Overlay message exceeds 32768 characters".into());
        }
        let font = self.font(language);
        let color = if size == 20 {
            [190, 190, 190]
        } else {
            [223, 223, 223]
        };
        let ruby_size = size / 2;
        let mut token_lines: Vec<Vec<(&str, &str, &str, f32)>> = Vec::new();
        let mut line = Vec::new();
        let mut line_width = 0.;
        for token in ruby {
            let orig = token.get("orig").and_then(Value::as_str).unwrap_or("");
            if orig.is_empty() {
                continue;
            }
            let hira = token.get("hira").and_then(Value::as_str).unwrap_or("");
            let roman = token.get("hepburn").and_then(Value::as_str).unwrap_or("");
            let w = font
                .width(orig, size as f32)
                .max(font.width(hira, ruby_size as f32))
                .max(font.width(roman, ruby_size as f32))
                .max(1.);
            if line_width + w > width as f32 * 0.9 && !line.is_empty() {
                token_lines.push(line);
                line = Vec::new();
                line_width = 0.;
            }
            line.push((orig, hira, roman, w));
            line_width += w;
        }
        if !line.is_empty() {
            token_lines.push(line);
        }
        if !token_lines.is_empty() {
            let mut rendered = Vec::new();
            for tokens in token_lines {
                let roman = tokens.iter().any(|t| !t.2.is_empty());
                let hira = tokens.iter().any(|t| !t.1.is_empty());
                let ruby_height = (usize::from(roman) + usize::from(hira)) * ruby_size
                    + if roman && hira { 4 } else { 0 };
                let mut frame =
                    Frame::blank(width, 20 + ruby_height + size + if small { 2 } else { 0 })?;
                let line_width = tokens.iter().map(|t| t.3).sum::<f32>();
                let mut x = match align {
                    Align::Center => (width as f32 - line_width) / 2.,
                    Align::Left => 0.,
                    Align::Right => width as f32 - line_width,
                };
                for (orig, hira_text, roman_text, w) in tokens {
                    let center = x + w / 2.;
                    let mut y = 10.;
                    if roman {
                        font.draw(
                            &mut frame,
                            roman_text,
                            ruby_size as f32,
                            center - font.width(roman_text, ruby_size as f32) / 2.,
                            y,
                            color,
                        );
                        y += ruby_size as f32 + if hira { 4. } else { 0. };
                    }
                    if hira {
                        font.draw(
                            &mut frame,
                            hira_text,
                            ruby_size as f32,
                            center - font.width(hira_text, ruby_size as f32) / 2.,
                            y,
                            color,
                        );
                    }
                    font.draw(
                        &mut frame,
                        orig,
                        size as f32,
                        center - font.width(orig, size as f32) / 2.,
                        (10 + ruby_height + if small { 2 } else { 0 }) as f32,
                        color,
                    );
                    x += w;
                }
                rendered.push(frame);
            }
            return stack(&rendered, width, 0, 0, 0);
        }
        let count = text.chars().count().max(1);
        let measured = text
            .split('\n')
            .map(|line| font.width(line, size as f32))
            .fold(0f32, f32::max);
        let average = (measured / count as f32).floor().max(1.);
        let length = ((width as f32 / average).floor() as usize)
            .saturating_sub(if small { 12 } else { 1 })
            .max(1);
        let lines: Vec<String> = text
            .split('\n')
            .flat_map(|line| {
                let chars: Vec<char> = line.chars().collect();
                if chars.is_empty() {
                    vec![String::new()]
                } else {
                    chars.chunks(length).map(|c| c.iter().collect()).collect()
                }
            })
            .collect();
        let height = if small {
            size * (lines.len() + 1) + 20
        } else {
            size * lines.len() + 10
        };
        let mut frame = Frame::blank(width, height)?;
        let top = (height - size * lines.len()) / 2;
        for (i, line) in lines.iter().enumerate() {
            let w = font.width(line, size as f32);
            let x = match align {
                Align::Center => (width as f32 - w) / 2.,
                Align::Left => 0.,
                Align::Right => width as f32 - w,
            };
            font.draw(
                &mut frame,
                line,
                size as f32,
                x,
                (top + i * size) as f32,
                color,
            );
        }
        Ok(frame)
    }
    pub fn small(&self, log: &SmallLog<'_>) -> Result<Frame, String> {
        let languages = enabled_languages(log.your_languages);
        let mut frames = Vec::new();
        if !log.translation.is_empty() && !languages.is_empty() {
            if let Some(message) = log.message.filter(|m| !m.is_empty()) {
                frames.push(self.textbox(
                    message,
                    log.language,
                    &[],
                    3840,
                    92,
                    true,
                    Align::Center,
                )?);
            }
            for (i, (text, language)) in log.translation.iter().zip(languages).enumerate() {
                frames.push(
                    self.textbox(
                        text,
                        Some(&language),
                        log.transliteration_translation
                            .get(i)
                            .map_or(&[], Vec::as_slice),
                        3840,
                        92,
                        true,
                        Align::Center,
                    )?,
                );
            }
        } else {
            frames.push(self.textbox(
                log.message.unwrap_or(""),
                log.language,
                log.transliteration_message,
                3840,
                92,
                true,
                Align::Center,
            )?);
        }
        stack(&frames, 3840, 0, 50, 50)
    }
    pub fn large(&mut self, log: &LargeLog<'_>, time: &str) -> Result<Frame, String> {
        let record = Record {
            direction: log.direction.to_owned(),
            message: log.message.map(str::to_owned),
            language: log.language.map(str::to_owned),
            translation: log.translation.to_vec(),
            languages: log.languages.clone(),
            ruby: log.transliteration_message.to_vec(),
            translation_ruby: log.transliteration_translation.to_vec(),
            time: time.to_owned(),
        };
        self.history.push_back(record);
        while self.history.len() > 5 {
            self.history.pop_front();
        }
        let mut frames = Vec::new();
        for row in &self.history {
            let receive = row.direction == "receive";
            let align = || if receive { Align::Left } else { Align::Right };
            let mut header = Frame::blank(960, 30)?;
            let font = self.font(None);
            let label = if receive { "Receive" } else { "Send" };
            let color = if receive {
                [168, 97, 180]
            } else {
                [97, 151, 180]
            };
            let tw = font.width(&row.time, 20.);
            let gap = tw / row.time.chars().count().max(1) as f32;
            let lw = font.width(label, 20.);
            font.draw(
                &mut header,
                &row.time,
                20.,
                if receive { 0. } else { 960. - tw - lw - gap },
                5.,
                [120, 120, 120],
            );
            font.draw(
                &mut header,
                label,
                20.,
                if receive { tw + gap } else { 960. - lw },
                5.,
                color,
            );
            let mut blocks = vec![header];
            let languages = enabled_languages(&row.languages);
            if !row.translation.is_empty() && !languages.is_empty() {
                if let Some(message) = &row.message {
                    blocks.push(self.textbox(
                        message,
                        row.language.as_deref(),
                        &[],
                        960,
                        20,
                        false,
                        align(),
                    )?);
                }
                for (i, (text, language)) in row.translation.iter().zip(languages).enumerate() {
                    blocks.push(self.textbox(
                        text,
                        Some(&language),
                        row.translation_ruby.get(i).map_or(&[], Vec::as_slice),
                        960,
                        30,
                        false,
                        align(),
                    )?);
                }
            } else {
                blocks.push(self.textbox(
                    row.message.as_deref().unwrap_or(""),
                    row.language.as_deref(),
                    &row.ruby,
                    960,
                    30,
                    false,
                    align(),
                )?);
            }
            frames.push(stack(&blocks, 960, 0, 0, 0)?);
        }
        stack(&frames, 960, 20, 25, 25)
    }
    pub fn clear_history(&mut self) {
        self.history.clear();
    }
    pub fn history_len(&self) -> usize {
        self.history.len()
    }
}
fn enabled_languages(value: &Value) -> Vec<String> {
    value
        .as_object()
        .into_iter()
        .flat_map(|o| o.values())
        .filter(|v| v.get("enable") == Some(&Value::Bool(true)))
        .filter_map(|v| v.get("language").and_then(Value::as_str).map(str::to_owned))
        .collect()
}
fn stack(
    frames: &[Frame],
    width: usize,
    spacing: usize,
    padding: usize,
    radius: i32,
) -> Result<Frame, String> {
    let height = frames.iter().map(|f| f.height as usize).sum::<usize>()
        + spacing * frames.len().saturating_sub(1)
        + padding * 2;
    let mut out = Frame::blank(width + padding * 2, height.max(1))?;
    if radius > 0 {
        out.background(radius);
    }
    let mut y = padding;
    for frame in frames {
        out.paste(frame, padding as i32, y as i32);
        y += frame.height as usize + spacing;
    }
    Ok(out)
}
