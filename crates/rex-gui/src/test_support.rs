//! Headless egui harness for GUI tests: runs frames without a window and lets tests
//! feed pointer/keyboard input and inspect what was drawn or copied.

use egui::{Event, FullOutput, Key, Modifiers, Pos2, Rect, Vec2};

pub struct Harness {
    pub ctx: egui::Context,
    size: Vec2,
    time: f64,
    pending: Vec<Event>,
    pointer: Pos2,
}

impl Harness {
    pub fn new(width: f32, height: f32) -> Self {
        Self {
            ctx: egui::Context::default(),
            size: Vec2::new(width, height),
            time: 0.0,
            pending: Vec::new(),
            pointer: Pos2::ZERO,
        }
    }

    /// Runs one 60 Hz frame with the queued input, drawing `ui_fn` in a central panel.
    pub fn frame(&mut self, mut ui_fn: impl FnMut(&mut egui::Ui)) -> FullOutput {
        self.time += 1.0 / 60.0;
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, self.size)),
            time: Some(self.time),
            predicted_dt: 1.0 / 60.0,
            events: std::mem::take(&mut self.pending),
            ..Default::default()
        };
        self.ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| ui_fn(ui));
        })
    }

    pub fn move_to(&mut self, pos: Pos2) {
        self.pointer = pos;
        self.pending.push(Event::PointerMoved(pos));
    }

    fn button(&mut self, pos: Pos2, pressed: bool) {
        self.move_to(pos);
        self.pending.push(Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        });
    }

    pub fn press(&mut self, pos: Pos2) {
        self.button(pos, true);
    }

    pub fn release(&mut self, pos: Pos2) {
        self.button(pos, false);
    }

    pub fn key(&mut self, key: Key) {
        for pressed in [true, false] {
            self.pending.push(Event::Key {
                key,
                physical_key: None,
                pressed,
                repeat: false,
                modifiers: Modifiers::NONE,
            });
        }
    }
}

/// Text placed on the clipboard during a frame, if any.
pub fn copied_text(out: &FullOutput) -> Option<String> {
    out.platform_output.commands.iter().find_map(|c| match c {
        egui::OutputCommand::CopyText(text) => Some(text.clone()),
        _ => None,
    })
}

/// Screen rectangle of the first drawn text equal to `needle` (or starting with it).
pub fn find_text(out: &FullOutput, needle: &str) -> Option<Rect> {
    out.shapes.iter().find_map(|clipped| match &clipped.shape {
        egui::Shape::Text(text) if text.galley.text().starts_with(needle) => {
            Some(text.galley.rect.translate(text.pos.to_vec2()))
        }
        _ => None,
    })
}
