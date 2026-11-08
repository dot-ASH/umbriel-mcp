use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: i64,
    pub y: i64,
    pub width: i64,
    pub height: i64,
}

impl Rect {
    fn right(&self) -> i64 {
        self.x + self.width
    }

    fn bottom(&self) -> i64 {
        self.y + self.height
    }

    fn area(&self) -> i64 {
        self.width.max(0) * self.height.max(0)
    }

    pub fn overlap(&self, other: &Rect) -> i64 {
        let width = self.right().min(other.right()) - self.x.max(other.x);
        let height = self.bottom().min(other.bottom()) - self.y.max(other.y);
        width.max(0) * height.max(0)
    }

    pub fn contains_rect(&self, other: &Rect) -> bool {
        other.x >= self.x
            && other.y >= self.y
            && other.right() <= self.right()
            && other.bottom() <= self.bottom()
    }

    pub fn contains_point(&self, x: i64, y: i64) -> bool {
        x >= self.x && y >= self.y && x < self.right() && y < self.bottom()
    }
}

fn number(entry: &Value, key: &str) -> Option<i64> {
    entry.get(key)?.as_i64()
}

pub fn window_rect(window: &Value) -> Option<Rect> {
    Some(Rect {
        x: number(window, "x")?,
        y: number(window, "y")?,
        width: number(window, "w")?,
        height: number(window, "h")?,
    })
}

pub fn output_rect(outputs: &[Value], name: &str) -> Option<Rect> {
    let output = outputs
        .iter()
        .find(|output| output.get("name").and_then(Value::as_str) == Some(name))?;
    let position = output.get("position")?;
    let size = output.get("logical_size")?;
    Some(Rect {
        x: number(position, "x")?,
        y: number(position, "y")?,
        width: number(size, "width")?,
        height: number(size, "height")?,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OnScreen {
    Hidden,
    ScrolledOutOfView,
    PartlyInView,
    FullyInView,
}

impl OnScreen {
    pub fn any_part_shown(self) -> bool {
        matches!(self, OnScreen::PartlyInView | OnScreen::FullyInView)
    }
}

fn flag(window: &Value, key: &str) -> Option<bool> {
    window.get(key).and_then(Value::as_bool)
}

pub fn on_screen(window: &Value, outputs: &[Value]) -> OnScreen {
    let on_active_workspace = flag(window, "visible").unwrap_or(true);
    if !on_active_workspace || flag(window, "minimized") == Some(true) {
        return OnScreen::Hidden;
    }
    let output = window
        .get("output")
        .and_then(Value::as_str)
        .and_then(|name| output_rect(outputs, name));
    let (Some(rect), Some(output)) = (window_rect(window), output) else {
        return OnScreen::FullyInView;
    };
    if output.contains_rect(&rect) {
        OnScreen::FullyInView
    } else if output.overlap(&rect) > 0 {
        OnScreen::PartlyInView
    } else {
        OnScreen::ScrolledOutOfView
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Covering {
    pub description: String,
    pub hides_most_of_it: bool,
    pub area: Rect,
}

const MOSTLY_HIDDEN_SHARE: f64 = 0.5;

fn covering(description: String, area: Rect, of: &Rect, modal: bool) -> Covering {
    let share = of.overlap(&area) as f64 / of.area().max(1) as f64;
    Covering {
        description,
        hides_most_of_it: modal || share >= MOSTLY_HIDDEN_SHARE,
        area,
    }
}

pub fn things_drawn_over(window: &Value, windows: &[Value], layers: &[Value]) -> Vec<Covering> {
    let Some(rect) = window_rect(window) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for layer in layers {
        let level = layer.get("layer").and_then(Value::as_str).unwrap_or("");
        let mapped = flag(layer, "mapped").unwrap_or(true);
        if !mapped || !matches!(level, "top" | "overlay") {
            continue;
        }
        let Some(layer_rect) = window_rect(layer) else {
            continue;
        };
        if rect.overlap(&layer_rect) == 0 {
            continue;
        }
        let modal =
            layer.get("keyboard_interactivity").and_then(Value::as_str) == Some("exclusive");
        let name = layer
            .get("namespace")
            .and_then(Value::as_str)
            .unwrap_or("unnamed");
        found.push(covering(
            format!("the {level}-layer surface \"{name}\""),
            layer_rect,
            &rect,
            modal,
        ));
    }
    let id = window.get("id").and_then(Value::as_str);
    let target_floats = flag(window, "floating") == Some(true);
    for other in windows {
        let same_workspace = other.get("workspace") == window.get("workspace");
        let drawn_above = flag(other, "fullscreen") == Some(true)
            || (flag(other, "floating") == Some(true) && !target_floats);
        if other.get("id").and_then(Value::as_str) == id
            || !same_workspace
            || !drawn_above
            || flag(other, "visible") == Some(false)
            || flag(other, "minimized") == Some(true)
        {
            continue;
        }
        let Some(other_rect) = window_rect(other) else {
            continue;
        };
        if rect.overlap(&other_rect) == 0 {
            continue;
        }
        let name = other.get("app_id").and_then(Value::as_str).unwrap_or("?");
        let other_id = other.get("id").and_then(Value::as_str).unwrap_or("?");
        found.push(covering(
            format!("window {other_id} ({name})"),
            other_rect,
            &rect,
            false,
        ));
    }
    found
}

#[cfg(test)]
mod tests {
    use super::{OnScreen, on_screen, things_drawn_over};
    use serde_json::json;

    fn outputs() -> Vec<serde_json::Value> {
        vec![json!({"name": "DP-1", "position": {"x": 1920, "y": 0},
                    "logical_size": {"width": 1920, "height": 1080}})]
    }

    fn window(x: i64, visible: bool) -> serde_json::Value {
        json!({"id": "w", "output": "DP-1", "workspace": "DP-1:1", "visible": visible,
               "x": x, "y": 40, "w": 900, "h": 600})
    }

    #[test]
    fn a_window_on_the_active_workspace_can_still_be_scrolled_out_of_view() {
        assert_eq!(
            on_screen(&window(2000, true), &outputs()),
            OnScreen::FullyInView
        );
        assert_eq!(
            on_screen(&window(3500, true), &outputs()),
            OnScreen::PartlyInView
        );
        assert_eq!(
            on_screen(&window(4200, true), &outputs()),
            OnScreen::ScrolledOutOfView
        );
        assert_eq!(
            on_screen(&window(2000, false), &outputs()),
            OnScreen::Hidden
        );
        let minimized = json!({"visible": true, "minimized": true});
        assert_eq!(on_screen(&minimized, &outputs()), OnScreen::Hidden);
    }

    #[test]
    fn without_output_details_umbriels_visible_flag_decides() {
        assert_eq!(on_screen(&window(9000, true), &[]), OnScreen::FullyInView);
    }

    #[test]
    fn a_modal_menu_or_a_large_overlay_hides_the_window_and_a_toast_only_overlaps_it() {
        let target = window(2000, true);
        let session_menu = json!({"layer": "overlay", "namespace": "session-menu", "mapped": true,
                                   "keyboard_interactivity": "exclusive",
                                   "x": 2300, "y": 300, "w": 300, "h": 200});
        let toast = json!({"layer": "overlay", "namespace": "toast", "mapped": true,
                           "keyboard_interactivity": "none",
                           "x": 2700, "y": 40, "w": 300, "h": 80});
        let wallpaper = json!({"layer": "background", "namespace": "wallpaper", "mapped": true,
                               "x": 1920, "y": 0, "w": 1920, "h": 1080});

        let found = things_drawn_over(&target, &[], &[session_menu, toast, wallpaper]);

        assert_eq!(found.len(), 2, "{found:?}");
        assert!(found[0].hides_most_of_it && found[0].description.contains("session-menu"));
        assert!(!found[1].hides_most_of_it && found[1].description.contains("toast"));
    }

    #[test]
    fn floating_windows_are_drawn_over_tiled_ones_on_the_same_workspace() {
        let target = window(2000, true);
        let floating = json!({"id": "f", "app_id": "pavucontrol", "workspace": "DP-1:1",
                              "floating": true, "visible": true,
                              "x": 2000, "y": 40, "w": 800, "h": 500});
        let elsewhere = json!({"id": "e", "workspace": "DP-1:2", "floating": true,
                               "x": 2000, "y": 40, "w": 800, "h": 500});

        let found = things_drawn_over(&target, &[target.clone(), floating, elsewhere], &[]);

        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].hides_most_of_it && found[0].description.contains("pavucontrol"));
    }
}
