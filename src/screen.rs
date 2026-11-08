use crate::placement::OnScreen;
use anyhow::{Context, Result, anyhow};
use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use image::{DynamicImage, ImageFormat};
use std::io::{Cursor, Write};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

pub const DEFAULT_MAX_WIDTH: u32 = 1024;
const JPEG_QUALITY: u8 = 80;
const MINIMUM_WORD_COLUMNS: usize = 12;
const WORD_GAP_LIMIT_IN_LINE_HEIGHTS: f32 = 1.2;
const FINGERPRINT_WIDTH: u32 = 256;
const PIXEL_DIFFERENCE_THAT_COUNTS: u8 = 24;
const CHANGED_PIXEL_SHARE: f64 = 0.0005;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Region {
    pub x: i64,
    pub y: i64,
    pub width: u32,
    pub height: u32,
}

impl Region {
    fn geometry(&self) -> String {
        format!("{},{} {}x{}", self.x, self.y, self.width, self.height)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Area {
    WholeScreen,
    Output(String),
    Window(String),
    Region(Region),
}

impl Area {
    pub fn from_choice(
        output: Option<String>,
        window_id: Option<String>,
        region: Option<Region>,
    ) -> Result<Self> {
        match (output, window_id, region) {
            (None, None, None) => Ok(Area::WholeScreen),
            (Some(name), None, None) => Ok(Area::Output(name)),
            (None, Some(id), None) => Ok(Area::Window(id)),
            (None, None, Some(region)) if region.width == 0 || region.height == 0 => {
                Err(anyhow!("a region needs a width and height above 0"))
            }
            (None, None, Some(region)) => Ok(Area::Region(region)),
            _ => Err(anyhow!(
                "give only one of output, window_id or region; they each choose what to capture"
            )),
        }
    }

    pub fn key(&self) -> String {
        match self {
            Area::WholeScreen => "screen".to_owned(),
            Area::Output(name) => format!("output {name}"),
            Area::Window(id) => format!("window {id}"),
            Area::Region(region) => format!("region {}", region.geometry()),
        }
    }

    pub fn describe(&self) -> String {
        match self {
            Area::WholeScreen => "the whole screen".to_owned(),
            Area::Output(name) => format!("monitor {name}"),
            Area::Window(id) => format!("window {id}"),
            Area::Region(region) => format!("the region {}", region.geometry()),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Fingerprint {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

impl Fingerprint {
    fn of(image: &DynamicImage) -> Self {
        let height = ((image.height() as f64 * FINGERPRINT_WIDTH as f64)
            / image.width().max(1) as f64)
            .round()
            .max(1.0) as u32;
        let small = image.thumbnail_exact(FINGERPRINT_WIDTH, height).to_luma8();
        Self {
            width: small.width(),
            height: small.height(),
            pixels: small.into_raw(),
        }
    }

    pub fn differs_from(&self, other: &Fingerprint) -> bool {
        if (self.width, self.height) != (other.width, other.height) {
            return true;
        }
        let changed = self
            .pixels
            .iter()
            .zip(&other.pixels)
            .filter(|(a, b)| a.abs_diff(**b) >= PIXEL_DIFFERENCE_THAT_COUNTS)
            .count();
        changed as f64 / self.pixels.len().max(1) as f64 > CHANGED_PIXEL_SHARE
    }
}

pub fn clock_time(moment: SystemTime) -> String {
    let since_epoch = moment.duration_since(UNIX_EPOCH).unwrap_or_default();
    let seconds_today = since_epoch.as_secs() % 86_400;
    format!(
        "{:02}:{:02}:{:02}.{:03} UTC",
        seconds_today / 3600,
        seconds_today / 60 % 60,
        seconds_today % 60,
        since_epoch.subsec_millis()
    )
}

pub struct Capture {
    decoded: DynamicImage,
    pub native_png: Vec<u8>,
    pub native_width: u32,
    pub native_height: u32,
    pub encoded_width: u32,
    pub encoded_height: u32,
    pub origin_x: i64,
    pub origin_y: i64,
    pub shown_on_screen: bool,
    pub captured_at: SystemTime,
    pub window_title: Option<String>,
    pub fingerprint: Fingerprint,
    pub notes: Vec<String>,
}

impl Capture {
    pub fn from_png(png: &[u8], origin: (i64, i64), max_width: u32) -> Result<Self> {
        let decoded = image::load_from_memory(png).context("could not decode the screenshot")?;
        let native_width = decoded.width();
        let native_height = decoded.height();
        let fingerprint = Fingerprint::of(&decoded);
        let (encoded_width, encoded_height) = if native_width > max_width && max_width > 0 {
            let height =
                (native_height as f64 * max_width as f64 / native_width as f64).round() as u32;
            (max_width, height.max(1))
        } else {
            (native_width, native_height)
        };

        Ok(Self {
            decoded,
            native_png: png.to_vec(),
            native_width,
            native_height,
            encoded_width,
            encoded_height,
            origin_x: origin.0,
            origin_y: origin.1,
            shown_on_screen: true,
            captured_at: SystemTime::now(),
            window_title: None,
            fingerprint,
            notes: Vec::new(),
        })
    }

    pub fn jpeg(&self) -> Result<Vec<u8>> {
        let scaled = if (self.encoded_width, self.encoded_height)
            == (self.native_width, self.native_height)
        {
            self.decoded.clone()
        } else {
            self.decoded.resize_exact(
                self.encoded_width,
                self.encoded_height,
                FilterType::Lanczos3,
            )
        };
        let mut jpeg = Vec::new();
        JpegEncoder::new_with_quality(&mut Cursor::new(&mut jpeg), JPEG_QUALITY)
            .encode_image(&scaled)?;
        Ok(jpeg)
    }

    pub fn coordinate_note(&self) -> String {
        let factor = self.native_width as f64 / self.encoded_width.max(1) as f64;
        let visibility: String = self.notes.iter().map(|note| format!(" {note}")).collect();
        format!(
            "Image is {}x{} covering a {}x{} region whose top-left is absolute ({}, {}). \
             To convert: screen_x = image_x * {:.4} + {}, screen_y = image_y * {:.4} + {}. \
             Captured at {}.{visibility}",
            self.encoded_width,
            self.encoded_height,
            self.native_width,
            self.native_height,
            self.origin_x,
            self.origin_y,
            factor,
            self.origin_x,
            factor,
            self.origin_y,
            clock_time(self.captured_at)
        )
    }

    pub fn screen_point(&self, image_x: u32, image_y: u32) -> (i64, i64) {
        (
            i64::from(image_x) + self.origin_x,
            i64::from(image_y) + self.origin_y,
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

pub fn grim_supports_toplevel_capture() -> bool {
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

fn visibility_note(window_id: &str, on_screen: OnScreen) -> Option<String> {
    match on_screen {
        OnScreen::FullyInView => None,
        OnScreen::Hidden => Some(
            "The window is not currently shown (its workspace is hidden or it is minimized), so \
             these screen coordinates only apply after switching to its workspace."
                .to_owned(),
        ),
        OnScreen::PartlyInView | OnScreen::ScrolledOutOfView => Some(format!(
            "The window is partly or entirely scrolled out of view, so some of these screen \
             coordinates are off screen; call focus_window with {window_id} to bring it into \
             view before clicking."
        )),
    }
}

const OLDER_GRIM: &str = "capturing a window by itself needs grim 1.5 or newer (`grim -T`); \
                          this grim can only capture what is displayed";

fn capture_from_screen(
    window_id: &str,
    placement: &crate::ipc::WindowPlacement,
) -> Result<(Vec<u8>, Vec<String>)> {
    match placement.on_screen {
        OnScreen::Hidden => {
            return Err(anyhow!(
                "window {window_id} is not currently shown, and {OLDER_GRIM}. Switch to its \
                 workspace first, or upgrade grim."
            ));
        }
        OnScreen::PartlyInView | OnScreen::ScrolledOutOfView => {
            return Err(anyhow!(
                "window {window_id} is partly or entirely scrolled out of view, and \
                 {OLDER_GRIM}. Call focus_window to bring it into view first, or upgrade grim."
            ));
        }
        OnScreen::FullyInView => {}
    }
    if let Some(cover) = placement
        .drawn_over
        .iter()
        .find(|cover| cover.hides_most_of_it)
    {
        return Err(anyhow!(
            "window {window_id} is covered by {}, and {OLDER_GRIM}, so the picture would show \
             that instead of the window. Close it first, or upgrade grim.",
            cover.description
        ));
    }
    let mut notes = vec![
        "This grim (older than 1.5) cannot capture a window by itself, so this is the screen \
         area the window occupies: anything drawn on top of it is included."
            .to_owned(),
    ];
    if !placement.drawn_over.is_empty() {
        let overlapping: Vec<&str> = placement
            .drawn_over
            .iter()
            .map(|cover| cover.description.as_str())
            .collect();
        notes.push(format!("Partly covered by {}.", overlapping.join(", ")));
    }
    Ok((grim(&["-g".into(), placement.geometry.clone()])?, notes))
}

fn capture_window(window_id: &str) -> Result<(Vec<u8>, crate::ipc::WindowPlacement, Vec<String>)> {
    let placement = crate::ipc::window_placement(window_id)?;
    if !grim_supports_toplevel_capture() {
        let (png, notes) = capture_from_screen(window_id, &placement)?;
        return Ok((png, placement, notes));
    }
    let png = grim(&["-T".into(), window_id.to_owned()]).map_err(|error| {
        if error.to_string().contains("cannot find toplevel") {
            anyhow!("no window with id {window_id} can be captured")
        } else {
            error
        }
    })?;
    let notes = visibility_note(window_id, placement.on_screen)
        .into_iter()
        .collect();
    Ok((png, placement, notes))
}

pub fn capture(area: &Area, max_width: u32) -> Result<Capture> {
    let (png, origin) = match area {
        Area::Window(id) => {
            let (png, placement, notes) = capture_window(id)?;
            let mut capture = Capture::from_png(&png, placement.origin, max_width)?;
            capture.shown_on_screen = placement.shown_on_screen;
            capture.window_title = placement.title;
            capture.notes = notes;
            return Ok(capture);
        }
        Area::Output(name) => (grim(&["-o".into(), name.to_string()])?, output_origin(name)),
        Area::Region(region) => (
            grim(&["-g".into(), region.geometry()])?,
            (region.x, region.y),
        ),
        Area::WholeScreen => (grim(&[])?, (0, 0)),
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

fn rows_top_to_bottom(mut words: Vec<RecognisedWord>) -> Vec<Vec<RecognisedWord>> {
    words.sort_by(|a, b| a.top.total_cmp(&b.top));
    let mut rows: Vec<Vec<RecognisedWord>> = Vec::new();
    for word in words {
        let joins_current_row = rows
            .last()
            .and_then(|row| row.first())
            .is_some_and(|first| {
                (word.top - first.top).abs() <= word.height.max(first.height) / 2.0
            });
        match rows.last_mut() {
            Some(row) if joins_current_row => row.push(word),
            _ => rows.push(vec![word]),
        }
    }
    for row in &mut rows {
        row.sort_by(|a, b| a.left.total_cmp(&b.left));
    }
    rows
}

fn phrases_in_reading_order(words: Vec<RecognisedWord>) -> Vec<Vec<RecognisedWord>> {
    let mut phrases: Vec<Vec<RecognisedWord>> = Vec::new();
    for word in rows_top_to_bottom(words).into_iter().flatten() {
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
    use super::{
        Area, Capture, Fingerprint, Region, clock_time, find_text, matches_in_tsv, read_text,
    };
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
        let preview = image::load_from_memory(&capture.jpeg().unwrap()).unwrap();
        assert_eq!(preview.width(), 1024);
    }

    fn region(width: u32, height: u32) -> Region {
        Region {
            x: 10,
            y: 20,
            width,
            height,
        }
    }

    #[test]
    fn a_capture_area_is_chosen_by_at_most_one_target() {
        assert_eq!(
            Area::from_choice(None, None, None).unwrap(),
            Area::WholeScreen
        );
        assert_eq!(
            Area::from_choice(None, None, Some(region(5, 6))).unwrap(),
            Area::Region(region(5, 6))
        );
        let both = Area::from_choice(Some("DP-1".into()), Some("w".into()), None);
        assert!(both.unwrap_err().to_string().contains("only one"));
        assert!(Area::from_choice(None, None, Some(region(0, 6))).is_err());
    }

    fn fingerprint_of(image: image::GrayImage) -> Fingerprint {
        Fingerprint::of(&DynamicImage::ImageLuma8(image))
    }

    #[test]
    fn a_blinking_caret_is_not_a_change_but_a_new_panel_is() {
        let page = image::GrayImage::from_pixel(1920, 1080, image::Luma([240]));
        let mut with_caret = page.clone();
        for y in 500..518 {
            with_caret.put_pixel(900, y, image::Luma([0]));
        }
        let mut with_panel = page.clone();
        for x in 0..400 {
            for y in 0..300 {
                with_panel.put_pixel(x, y, image::Luma([40]));
            }
        }
        let page = fingerprint_of(page);
        assert!(!fingerprint_of(with_caret).differs_from(&page));
        assert!(fingerprint_of(with_panel).differs_from(&page));
    }

    #[test]
    fn capture_times_are_shown_to_the_millisecond() {
        let moment =
            std::time::UNIX_EPOCH + std::time::Duration::from_millis(86_400_000 + 3_723_045);
        assert_eq!(clock_time(moment), "01:02:03.045 UTC");
    }

    #[test]
    fn a_dense_page_with_slightly_uneven_baselines_is_read_without_panicking() {
        let mut tsv = String::from("header\n");
        for index in 0..400u32 {
            let top = index / 2 + (index % 7);
            let left = (index * 37) % 1800;
            tsv.push_str(&tsv_row(left, top, 30, 10, 90, &format!("w{index}")));
            tsv.push('\n');
        }
        tsv.push_str(&tsv_row(900, 600, 40, 10, 95, "History"));
        tsv.push('\n');

        let found = matches_in_tsv(&tsv, "history");

        assert_eq!(found.len(), 1);
        assert_near((found[0].center_x, found[0].center_y), (920, 605));
    }

    #[test]
    fn words_on_one_line_are_read_left_to_right_despite_uneven_baselines() {
        let tsv = [
            "header".to_owned(),
            tsv_row(200, 102, 40, 12, 90, "File"),
            tsv_row(100, 100, 40, 12, 90, "Open"),
            tsv_row(150, 104, 40, 12, 90, "the"),
        ]
        .join("\n");

        assert_eq!(matches_in_tsv(&tsv, "open the file").len(), 1);
    }
}
