use anyhow::{Context, Result, anyhow};
use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use image::{DynamicImage, ImageFormat};
use std::io::{Cursor, Write};
use std::process::{Command, Stdio};
use std::sync::OnceLock;

pub const DEFAULT_MAX_WIDTH: u32 = 1024;
const JPEG_QUALITY: u8 = 80;
const MINIMUM_WORD_COLUMNS: usize = 12;
const WORD_GAP_LIMIT_IN_LINE_HEIGHTS: f32 = 1.2;

pub struct Capture {
    pub jpeg: Vec<u8>,
    pub native_png: Vec<u8>,
    pub native_width: u32,
    pub native_height: u32,
    pub encoded_width: u32,
    pub encoded_height: u32,
    pub origin_x: i64,
    pub origin_y: i64,
    pub shown_on_screen: bool,
}

impl Capture {
    pub fn from_png(png: &[u8], origin: (i64, i64), max_width: u32) -> Result<Self> {
        let decoded = image::load_from_memory(png).context("could not decode the screenshot")?;
        let native_width = decoded.width();
        let native_height = decoded.height();

        let scaled = if native_width > max_width && max_width > 0 {
            let height =
                (native_height as f64 * max_width as f64 / native_width as f64).round() as u32;
            decoded.resize(max_width, height.max(1), FilterType::Lanczos3)
        } else {
            decoded
        };

        let mut jpeg = Vec::new();
        JpegEncoder::new_with_quality(&mut Cursor::new(&mut jpeg), JPEG_QUALITY)
            .encode_image(&scaled)?;

        Ok(Self {
            jpeg,
            native_png: png.to_vec(),
            native_width,
            native_height,
            encoded_width: scaled.width(),
            encoded_height: scaled.height(),
            origin_x: origin.0,
            origin_y: origin.1,
            shown_on_screen: true,
        })
    }

    pub fn coordinate_note(&self) -> String {
        let factor = self.native_width as f64 / self.encoded_width.max(1) as f64;
        let visibility = if self.shown_on_screen {
            ""
        } else {
            " The window is not currently shown (its workspace is hidden or it is minimized), \
             so these screen coordinates only apply after switching to its workspace."
        };
        format!(
            "Image is {}x{} covering a {}x{} region whose top-left is absolute ({}, {}). \
             To convert: screen_x = image_x * {:.4} + {}, screen_y = image_y * {:.4} + {}.{visibility}",
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

fn grim_supports_toplevel_capture() -> bool {
    static SUPPORTED: OnceLock<bool> = OnceLock::new();
    *SUPPORTED.get_or_init(|| {
        Command::new("grim")
            .arg("-h")
            .output()
            .map(|output| {
                let help = [output.stdout, output.stderr].concat();
                String::from_utf8_lossy(&help).contains("-T <")
            })
            .unwrap_or(false)
    })
}

fn capture_window(window_id: &str) -> Result<(Vec<u8>, crate::ipc::WindowPlacement)> {
    let placement = crate::ipc::window_placement(window_id)?;
    let png = if grim_supports_toplevel_capture() {
        grim(&["-T".into(), window_id.to_owned()]).map_err(|error| {
            if error.to_string().contains("cannot find toplevel") {
                anyhow!("no window with id {window_id} can be captured")
            } else {
                error
            }
        })?
    } else if placement.shown_on_screen {
        grim(&["-g".into(), placement.geometry.clone()])?
    } else {
        return Err(anyhow!(
            "window {window_id} is not currently shown, and capturing a window that is not on \
             screen needs grim 1.5 or newer (`grim -T`); this grim can only capture what is \
             displayed. Switch to its workspace first, or upgrade grim."
        ));
    };
    Ok((png, placement))
}

pub fn capture(output: Option<&str>, window_id: Option<&str>, max_width: u32) -> Result<Capture> {
    if let Some(id) = window_id {
        let (png, placement) = capture_window(id)?;
        let mut capture = Capture::from_png(&png, placement.origin, max_width)?;
        capture.shown_on_screen = placement.shown_on_screen;
        return Ok(capture);
    }
    let (png, origin) = match output {
        Some(name) => (grim(&["-o".into(), name.to_string()])?, output_origin(name)),
        None => (grim(&[])?, (0, 0)),
    };
    Capture::from_png(&png, origin, max_width)
}

const TESSERACT_INSTALL_ADVICE: &str = "run `umbriel-mcp doctor` to see what to install";

fn grayscale_png(image_bytes: &[u8]) -> Result<Vec<u8>> {
    let gray = DynamicImage::ImageLuma8(
        image::load_from_memory(image_bytes)
            .context("could not decode the image for text recognition")?
            .to_luma8(),
    );
    let mut png = Vec::new();
    gray.write_to(&mut Cursor::new(&mut png), ImageFormat::Png)?;
    Ok(png)
}

fn explain_tesseract_failure(stderr: &str) -> anyhow::Error {
    let detail = stderr.trim();
    if detail.contains("traineddata") || detail.contains("tessdata") {
        anyhow!(
            "tesseract has no English language data ({detail}); install tesseract-data-eng (Arch), tesseract-ocr-eng (Debian/Ubuntu) or tesseract-langpack-eng (Fedora)"
        )
    } else {
        anyhow!("tesseract failed: {detail}")
    }
}

fn tesseract(image_bytes: &[u8], extra: &[&str]) -> Result<String> {
    let png = grayscale_png(image_bytes)?;
    let mut child = Command::new("tesseract")
        .args(["stdin", "stdout"])
        .args(extra)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|cause| anyhow!("cannot run tesseract ({cause}); {TESSERACT_INSTALL_ADVICE}"))?;
    let mut stdin = child.stdin.take().context("tesseract has no stdin")?;
    let writer = std::thread::spawn(move || stdin.write_all(&png));
    let output = child
        .wait_with_output()
        .context("could not wait for tesseract")?;
    let _ = writer.join();
    if !output.status.success() {
        return Err(explain_tesseract_failure(&String::from_utf8_lossy(
            &output.stderr,
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
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

struct RecognisedWord {
    text: String,
    left: f32,
    top: f32,
    width: f32,
    height: f32,
    confidence: f32,
}

impl RecognisedWord {
    fn right(&self) -> f32 {
        self.left + self.width
    }

    fn bottom(&self) -> f32 {
        self.top + self.height
    }

    fn continues_phrase_of(&self, previous: &RecognisedWord) -> bool {
        let line_height = self.height.max(previous.height);
        let on_same_baseline = (self.top - previous.top).abs() <= line_height / 2.0;
        let gap = self.left - previous.right();
        on_same_baseline
            && gap >= -line_height
            && gap <= line_height * WORD_GAP_LIMIT_IN_LINE_HEIGHTS
    }
}

fn recognised_words(tsv: &str) -> Vec<RecognisedWord> {
    tsv.lines()
        .skip(1)
        .filter_map(|line| {
            let columns: Vec<&str> = line.split('\t').collect();
            if columns.len() < MINIMUM_WORD_COLUMNS {
                return None;
            }
            let text = columns[11].trim();
            let number = |index: usize| columns[index].parse::<f32>().unwrap_or(-1.0);
            let confidence = number(10);
            if text.is_empty() || confidence < 0.0 {
                return None;
            }
            Some(RecognisedWord {
                text: text.to_owned(),
                left: number(6),
                top: number(7),
                width: number(8),
                height: number(9),
                confidence,
            })
        })
        .collect()
}

fn phrases_in_reading_order(mut words: Vec<RecognisedWord>) -> Vec<Vec<RecognisedWord>> {
    words.sort_by(|a, b| {
        let same_row = (a.top - b.top).abs() <= a.height.max(b.height) / 2.0;
        if same_row {
            a.left.total_cmp(&b.left)
        } else {
            a.top.total_cmp(&b.top)
        }
    });
    let mut phrases: Vec<Vec<RecognisedWord>> = Vec::new();
    for word in words {
        match phrases.last_mut() {
            Some(phrase)
                if phrase
                    .last()
                    .is_some_and(|last| word.continues_phrase_of(last)) =>
            {
                phrase.push(word)
            }
            _ => phrases.push(vec![word]),
        }
    }
    phrases
}

fn matches_in_phrase(phrase: &[RecognisedWord], wanted: &str) -> Vec<TextMatch> {
    let mut joined = String::new();
    let mut spans = Vec::new();
    for word in phrase {
        if !joined.is_empty() {
            joined.push(' ');
        }
        let start = joined.len();
        joined.push_str(&word.text.to_lowercase());
        spans.push(start..joined.len());
    }
    joined
        .match_indices(wanted)
        .map(|(start, found)| {
            let end = start + found.len();
            let covered: Vec<&RecognisedWord> = phrase
                .iter()
                .zip(&spans)
                .filter(|(_, span)| span.start < end && span.end > start)
                .map(|(word, _)| word)
                .collect();
            let left = covered.iter().map(|w| w.left).fold(f32::MAX, f32::min);
            let top = covered.iter().map(|w| w.top).fold(f32::MAX, f32::min);
            let right = covered.iter().map(|w| w.right()).fold(f32::MIN, f32::max);
            let bottom = covered.iter().map(|w| w.bottom()).fold(f32::MIN, f32::max);
            let confidence =
                covered.iter().map(|w| w.confidence).sum::<f32>() / covered.len() as f32;
            TextMatch {
                text: covered
                    .iter()
                    .map(|w| w.text.as_str())
                    .collect::<Vec<_>>()
                    .join(" "),
                confidence,
                center_x: ((left + right) / 2.0).max(0.0) as u32,
                center_y: ((top + bottom) / 2.0).max(0.0) as u32,
            }
        })
        .collect()
}

pub fn matches_in_tsv(tsv: &str, needle: &str) -> Vec<TextMatch> {
    let wanted = needle
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    if wanted.is_empty() {
        return Vec::new();
    }
    phrases_in_reading_order(recognised_words(tsv))
        .iter()
        .flat_map(|phrase| matches_in_phrase(phrase, &wanted))
        .collect()
}

pub fn find_text(image_bytes: &[u8], needle: &str) -> Result<Vec<TextMatch>> {
    let tsv = tesseract(image_bytes, &["--psm", "11", "tsv"])?;
    Ok(matches_in_tsv(&tsv, needle))
}

#[cfg(test)]
mod tests {
    use super::{Capture, find_text, matches_in_tsv, read_text};
    use image::{DynamicImage, ImageFormat};
    use std::io::Cursor;

    const WIDE_UI_FIXTURE: &[u8] = include_bytes!("../tests/fixtures/ocr-ui-wide.png");

    fn assert_near(actual: (u32, u32), expected: (u32, u32)) {
        let close = |a: u32, b: u32| a.abs_diff(b) <= 8;
        assert!(
            close(actual.0, expected.0) && close(actual.1, expected.1),
            "expected near {expected:?}, got {actual:?}"
        );
    }

    fn tsv_row(
        left: u32,
        top: u32,
        width: u32,
        height: u32,
        confidence: i32,
        word: &str,
    ) -> String {
        format!("5\t1\t1\t1\t1\t1\t{left}\t{top}\t{width}\t{height}\t{confidence}\t{word}")
    }

    fn tsv(rows: &[String]) -> String {
        let header = "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext";
        std::iter::once(header.to_owned())
            .chain(rows.iter().cloned())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn reading_a_full_width_capture_finds_every_line_including_button_labels() {
        let text =
            read_text(WIDE_UI_FIXTURE).expect("tesseract with English data must be installed");

        for expected in [
            "Umbriel OCR Probe",
            "Settings",
            "Save",
            "Cancel",
            "quick brown fox 12345",
            "Open File",
        ] {
            assert!(
                text.contains(expected),
                "missing {expected:?} in OCR text:\n{text}"
            );
        }
    }

    #[test]
    fn a_button_label_is_found_at_its_native_pixel_position() {
        let found = find_text(WIDE_UI_FIXTURE, "cancel").expect("ocr");

        assert_eq!(
            found.len(),
            1,
            "{:?}",
            found.iter().map(|m| &m.text).collect::<Vec<_>>()
        );
        assert_near((found[0].center_x, found[0].center_y), (319, 174));
        assert!(found[0].confidence > 50.0);
    }

    #[test]
    fn a_multi_word_phrase_is_found_as_one_match_centered_on_the_phrase() {
        let found = find_text(WIDE_UI_FIXTURE, "Open File").expect("ocr");

        assert_eq!(found.len(), 1);
        assert_near((found[0].center_x, found[0].center_y), (96, 352));
        assert_eq!(found[0].text, "Open File");
    }

    #[test]
    fn neighbouring_buttons_are_not_glued_into_one_phrase() {
        assert!(
            find_text(WIDE_UI_FIXTURE, "Settings Save")
                .expect("ocr")
                .is_empty()
        );
        assert!(
            find_text(WIDE_UI_FIXTURE, "no such label")
                .expect("ocr")
                .is_empty()
        );
    }

    #[test]
    fn a_target_matches_inside_a_longer_word_and_ignores_case() {
        let found = find_text(WIDE_UI_FIXTURE, "SETTING").expect("ocr");

        assert_eq!(found.len(), 1);
        assert_eq!(found[0].text, "Settings");
    }

    #[test]
    fn matches_come_from_each_occurrence_in_reading_order() {
        let rows = tsv(&[
            tsv_row(10, 10, 40, 12, 90, "Save"),
            tsv_row(10, 100, 40, 12, 90, "Save"),
        ]);

        let found = matches_in_tsv(&rows, "save");

        assert_eq!(found.len(), 2);
        assert_eq!((found[0].center_x, found[0].center_y), (30, 16));
        assert_eq!((found[1].center_x, found[1].center_y), (30, 106));
    }

    #[test]
    fn adjacent_words_on_one_baseline_form_a_phrase_but_distant_ones_do_not() {
        let rows = tsv(&[
            tsv_row(10, 50, 40, 12, 80, "Open"),
            tsv_row(54, 50, 30, 12, 90, "File"),
            tsv_row(400, 50, 40, 12, 90, "Close"),
        ]);

        assert_eq!(matches_in_tsv(&rows, "open file").len(), 1);
        assert!(matches_in_tsv(&rows, "file close").is_empty());
    }

    #[test]
    fn a_phrase_match_reports_the_average_confidence_of_its_words() {
        let rows = tsv(&[
            tsv_row(10, 50, 40, 12, 80, "Open"),
            tsv_row(54, 50, 30, 12, 90, "File"),
        ]);

        let found = matches_in_tsv(&rows, "Open File");

        assert!((found[0].confidence - 85.0).abs() < 0.01);
    }

    #[test]
    fn unrecognised_rows_and_negative_confidence_are_ignored() {
        let rows = tsv(&[
            "4\t1\t1\t1\t1\t0\t10\t10\t100\t12\t-1\t".to_owned(),
            tsv_row(10, 10, 40, 12, -1, "Save"),
            "short\trow".to_owned(),
            tsv_row(10, 40, 40, 12, 95, ""),
        ]);

        assert!(matches_in_tsv(&rows, "save").is_empty());
    }

    #[test]
    fn a_capture_keeps_the_native_image_for_text_recognition_and_downscales_only_the_preview() {
        let mut png = Vec::new();
        DynamicImage::new_rgb8(3840, 1080)
            .write_to(&mut Cursor::new(&mut png), ImageFormat::Png)
            .unwrap();

        let capture = Capture::from_png(&png, (1920, 0), 1024).unwrap();

        assert_eq!((capture.native_width, capture.native_height), (3840, 1080));
        assert_eq!((capture.encoded_width, capture.encoded_height), (1024, 288));
        assert_eq!((capture.origin_x, capture.origin_y), (1920, 0));
        let native = image::load_from_memory(&capture.native_png).unwrap();
        assert_eq!((native.width(), native.height()), (3840, 1080));
        let preview = image::load_from_memory(&capture.jpeg).unwrap();
        assert_eq!(preview.width(), 1024);
    }
}
