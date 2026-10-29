use anyhow::{Result, anyhow};
use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use std::io::Cursor;
use std::process::Command;

pub const DEFAULT_MAX_WIDTH: u32 = 1024;
const JPEG_QUALITY: u8 = 80;

pub struct Capture {
    pub jpeg: Vec<u8>,
    pub native_width: u32,
    pub native_height: u32,
    pub encoded_width: u32,
    pub encoded_height: u32,
    pub origin_x: i64,
    pub origin_y: i64,
}

impl Capture {
    pub fn coordinate_note(&self) -> String {
        let factor = self.native_width as f64 / self.encoded_width.max(1) as f64;
        format!(
            "Image is {}x{} covering a {}x{} region whose top-left is absolute ({}, {}). \
             To convert: screen_x = image_x * {:.4} + {}, screen_y = image_y * {:.4} + {}.",
            self.encoded_width,
            self.encoded_height,
            self.native_width,
            self.native_height,
            self.origin_x,
            self.origin_y,
            factor,
            self.origin_x,
            factor,
            self.origin_y
        )
    }
}

fn grim(extra_args: &[String]) -> Result<Vec<u8>> {
    let mut args: Vec<String> = vec!["-t".into(), "png".into(), "-l".into(), "0".into()];
    args.extend_from_slice(extra_args);
    args.push("-".into());

    let result = Command::new("grim")
        .args(&args)
        .output()
        .map_err(|cause| anyhow!("cannot run grim: {cause}"))?;
    if !result.status.success() {
        return Err(anyhow!(
            "grim failed: {}",
            String::from_utf8_lossy(&result.stderr).trim()
        ));
    }
    if result.stdout.is_empty() {
        return Err(anyhow!("grim produced no image data"));
    }
    Ok(result.stdout)
}

fn parse_origin(geometry: &str) -> (i64, i64) {
    let position = geometry.split_whitespace().next().unwrap_or("0,0");
    let mut parts = position.split(',');
    let x = parts.next().and_then(|v| v.parse().ok()).unwrap_or(0);
    let y = parts.next().and_then(|v| v.parse().ok()).unwrap_or(0);
    (x, y)
}

fn output_origin(output_name: &str) -> (i64, i64) {
    let Ok(outputs) = crate::ipc::query("outputs") else {
        return (0, 0);
    };
    let Some(entries) = outputs.as_array() else {
        return (0, 0);
    };
    for entry in entries {
        if entry.get("name").and_then(|v| v.as_str()) == Some(output_name) {
            let position = entry.get("position");
            let read = |key: &str| {
                position
                    .and_then(|p| p.get(key))
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0)
            };
            return (read("x"), read("y"));
        }
    }
    (0, 0)
}

pub fn capture(output: Option<&str>, window_id: Option<&str>, max_width: u32) -> Result<Capture> {
    let (png, origin) = match (output, window_id) {
        (_, Some(id)) => {
            let geometry = crate::ipc::window_geometry(id)?;
            let origin = parse_origin(&geometry);
            (grim(&["-g".into(), geometry])?, origin)
        }
        (Some(name), None) => (grim(&["-o".into(), name.to_string()])?, output_origin(name)),
        (None, None) => (grim(&[])?, (0, 0)),
    };

    let decoded = image::load_from_memory(&png)?;
    let native_width = decoded.width();
    let native_height = decoded.height();

    let scaled = if native_width > max_width && max_width > 0 {
        let height = (native_height as f64 * max_width as f64 / native_width as f64).round() as u32;
        decoded.resize(max_width, height.max(1), FilterType::Lanczos3)
    } else {
        decoded
    };

    let mut jpeg = Vec::new();
    JpegEncoder::new_with_quality(&mut Cursor::new(&mut jpeg), JPEG_QUALITY)
        .encode_image(&scaled)?;

    Ok(Capture {
        jpeg,
        native_width,
        native_height,
        encoded_width: scaled.width(),
        encoded_height: scaled.height(),
        origin_x: origin.0,
        origin_y: origin.1,
    })
}

fn tesseract(image_bytes: &[u8], extra: &[&str]) -> Result<String> {
    let scratch = std::env::temp_dir().join(format!("umbriel-mcp-ocr-{}.png", std::process::id()));
    std::fs::write(&scratch, image_bytes)?;

    let mut args: Vec<String> = vec![scratch.to_string_lossy().into_owned(), "stdout".into()];
    args.extend(extra.iter().map(|value| value.to_string()));

    let result = Command::new("tesseract").args(&args).output();
    let _ = std::fs::remove_file(&scratch);

    let result = result.map_err(|cause| anyhow!("cannot run tesseract: {cause}"))?;
    if !result.status.success() {
        return Err(anyhow!(
            "tesseract failed: {}",
            String::from_utf8_lossy(&result.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&result.stdout).into_owned())
}

pub fn read_text(image_bytes: &[u8]) -> Result<String> {
    Ok(tesseract(image_bytes, &[])?.trim().to_string())
}

pub struct TextMatch {
    pub text: String,
    pub confidence: f32,
    pub center_x: u32,
    pub center_y: u32,
}

pub fn find_text(image_bytes: &[u8], needle: &str) -> Result<Vec<TextMatch>> {
    let tsv = tesseract(image_bytes, &["tsv"])?;
    let wanted = needle.to_lowercase();
    let mut matches = Vec::new();

    for line in tsv.lines().skip(1) {
        let columns: Vec<&str> = line.split('\t').collect();
        if columns.len() < 12 {
            continue;
        }
        let text = columns[11].trim();
        if text.is_empty() || !text.to_lowercase().contains(&wanted) {
            continue;
        }
        let number = |index: usize| columns[index].parse::<f32>().unwrap_or(-1.0);
        let (left, top, width, height, confidence) =
            (number(6), number(7), number(8), number(9), number(10));
        if confidence < 0.0 {
            continue;
        }
        matches.push(TextMatch {
            text: text.to_string(),
            confidence,
            center_x: (left + width / 2.0).max(0.0) as u32,
            center_y: (top + height / 2.0).max(0.0) as u32,
        });
    }
    Ok(matches)
}
