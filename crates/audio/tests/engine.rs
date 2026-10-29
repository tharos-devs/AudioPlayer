use std::path::Path;
use std::time::{Duration, Instant};

use audioplayer_audio::{Engine, Event};

// WAV PCM 16 bits mono silencieux : le test ne fait aucun bruit.
fn silent_wav(path: &Path, seconds: f32) {
    let rate = 44_100u32;
    let samples = (rate as f32 * seconds) as u32;
    let data = samples * 2;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&rate.to_le_bytes());
    bytes.extend_from_slice(&(rate * 2).to_le_bytes());
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data.to_le_bytes());
    bytes.resize(bytes.len() + data as usize, 0);
    std::fs::write(path, bytes).unwrap();
}

// Attend les événements jusqu'à `until` ; `None` si la machine n'a pas de sortie audio.
fn collect(engine: &Engine, until: impl Fn(&[Event]) -> bool) -> Option<Vec<Event>> {
    let start = Instant::now();
    let mut events = Vec::new();
    while start.elapsed() < Duration::from_secs(5) {
        events.extend(engine.events());
        if events.iter().any(|e| matches!(e, Event::NoOutput { .. })) {
            eprintln!("no audio output: skipped");
            return None;
        }
        if until(&events) {
            return Some(events);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("timeout, events so far: {events:?}");
}

#[test]
fn plays_then_chains_the_queued_track_then_ends() {
    let dir = tempfile::tempdir().unwrap();
    let (first, second) = (dir.path().join("a.wav"), dir.path().join("b.wav"));
    silent_wav(&first, 0.3);
    silent_wav(&second, 0.3);

    let engine = Engine::start();
    engine.set_volume(0.0);
    engine.play(&first, Some(&second));
    let Some(events) = collect(&engine, |events| events.contains(&Event::Ended)) else { return };
    let started: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            Event::Started { path, .. } => Some(path.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(started, [first, second]);
    assert_eq!(events.last(), Some(&Event::Ended));
}

#[test]
fn missing_file_is_reported() {
    let engine = Engine::start();
    let missing = Path::new("/nonexistent/track.flac");
    engine.play(missing, None);
    let Some(events) = collect(&engine, |events| events.iter().any(|e| matches!(e, Event::Failed { .. }))) else { return };
    assert!(matches!(&events[0], Event::Failed { path, .. } if path == missing));
}
