//! Zoom global de l'interface, équivalent du `zoom` CSS de la v0.1 : les tailles
//! des fichiers .slint sont les valeurs CSS d'origine, et le facteur d'échelle de
//! la fenêtre est multiplié par le niveau de zoom.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use slint::platform::WindowEvent;
use slint::{ComponentHandle, Timer, TimerMode};

/// Niveau par défaut de la v0.1 (affiché « 100 % »).
pub const DEFAULT: f32 = 0.9;
pub const MIN: f32 = 0.7;
pub const MAX: f32 = 0.9;

pub struct Zoom {
    level: Cell<f32>,
    // Facteur de l'écran, tel que fourni par le système.
    device: Cell<f32>,
    // Dernier facteur appliqué ici ; tout autre valeur vient du système.
    applied: Cell<Option<f32>>,
    watchdog: Timer,
}

impl Zoom {
    pub fn new() -> Rc<Self> {
        Rc::new(Zoom { level: Cell::new(DEFAULT), device: Cell::new(1.0), applied: Cell::new(None), watchdog: Timer::default() })
    }

    /// À appeler une fois la fenêtre affichée.
    pub fn attach<C: ComponentHandle + 'static>(self: &Rc<Self>, component: &C) {
        let weak = component.as_weak();
        let zoom = Rc::downgrade(self);
        // Changer d'écran rétablit le facteur du système : on le détecte et on réapplique.
        self.watchdog.start(TimerMode::Repeated, Duration::from_millis(500), move || {
            if let (Some(component), Some(zoom)) = (weak.upgrade(), zoom.upgrade()) {
                zoom.apply(component.window());
            }
        });
        self.apply(component.window());
    }

    pub fn level(&self) -> f32 {
        self.level.get()
    }

    pub fn set_level(&self, window: &slint::Window, level: f32) {
        self.level.set(level.clamp(MIN, MAX));
        self.apply(window);
    }

    fn apply(&self, window: &slint::Window) {
        let current = window.scale_factor();
        let same = |a: f32, b: f32| (a - b).abs() < 1e-4;
        if self.applied.get().is_none_or(|applied| !same(applied, current)) {
            self.device.set(current);
        }
        let scale_factor = self.device.get() * self.level.get();
        if same(scale_factor, current) {
            return;
        }
        self.applied.set(Some(scale_factor));
        let size = window.size();
        window.dispatch_event(WindowEvent::ScaleFactorChanged { scale_factor });
        window.dispatch_event(WindowEvent::Resized { size: size.to_logical(scale_factor) });
    }
}
