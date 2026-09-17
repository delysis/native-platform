//! Bounded native transport for EASL's shared nearest-caret policy. Shaping
//! remains native; no text, document transactions or application state enter
//! this VM. A pointer operation streams only the indexed line's caret stops.
use crate::{Error, runtime::Runtime};
use easl_native_text::{CaretStop, TextEditor, TextSystem, parley::Affinity};

const BATCH: usize = 256;
const WORDS: usize = 9;
const NONE: [u32; WORDS] = [2, 0, 1, 0, 0, 0, 0, 0, 0];

pub struct HitTesting {
    runtime: Runtime,
    words: Vec<u32>,
    best: [u32; WORDS],
    stop_count: usize,
    faulted: bool,
    pub decisions: u64,
}
impl std::fmt::Debug for HitTesting {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HitTesting")
            .field("faulted", &self.faulted)
            .field("decisions", &self.decisions)
            .finish_non_exhaustive()
    }
}
impl HitTesting {
    pub fn new() -> Result<Self, Error> {
        let runtime = std::panic::catch_unwind(|| {
            Runtime::compile(&[
                (
                    "hit-native.easl",
                    include_str!("../library/hit-native.easl"),
                ),
                ("geometry.easl", include_str!("../library/geometry.easl")),
                ("atlas.easl", include_str!("../library/atlas.easl")),
            ])
        })
        .map_err(|_| Error::Language("Hit-testing compiler panicked".into()))??;
        Ok(Self {
            runtime,
            words: Vec::with_capacity(BATCH * WORDS),
            best: NONE,
            stop_count: 0,
            faulted: false,
            decisions: 0,
        })
    }

    /// Choose a caret synchronously from native shaped geometry. Failed input
    /// or evaluation leaves the widget selection, text and history untouched.
    /// Active preedit must first be committed or cancelled by the host.
    pub fn hit_editor(
        &mut self,
        editor: &mut TextEditor,
        system: &mut TextSystem,
        point: [f32; 2],
    ) -> Result<CaretStop, Error> {
        self.evaluate(point, |this| {
            editor.visit_caret_stops(system, point[1], |stop| this.push(stop))
        })
    }

    /// Choose a caret on an explicitly selected line using the same policy as
    /// pointer input, without allowing adjacent line bands to change the target.
    pub fn hit_line(
        &mut self,
        editor: &mut TextEditor,
        system: &mut TextSystem,
        line: usize,
        x: f32,
    ) -> Result<CaretStop, Error> {
        let band = editor.line_geometry(system, line)?;
        let point = [x, (band.top + band.bottom) * 0.5];
        self.evaluate(point, |this| {
            editor.visit_line_caret_stops(system, line, |stop| this.push(stop))
        })
    }

    /// A reusable entry for other native layout engines. Stops are visited in
    /// the supplied order. Ties prefer the owning cluster containing the pointer,
    /// then downstream affinity. At most 2,097,154 stops may enter one query.
    pub fn hit_stops(
        &mut self,
        stops: impl IntoIterator<Item = CaretStop>,
        point: [f32; 2],
    ) -> Result<CaretStop, Error> {
        self.evaluate(point, |this| {
            for stop in stops {
                this.push(stop)?;
            }
            Ok(())
        })
    }

    fn evaluate(
        &mut self,
        point: [f32; 2],
        supply: impl FnOnce(&mut Self) -> Result<(), Error>,
    ) -> Result<CaretStop, Error> {
        if self.faulted {
            return Err(Error::Faulted);
        }
        if point.iter().any(|v| !v.is_finite() || v.abs() > 1e30) {
            return Err(Error::Invalid);
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.words.clear();
            self.best = NONE;
            self.stop_count = 0;
            self.runtime
                .write("text-hit-point", &point.map(f32::to_bits))?;
            self.runtime.write("text-hit-best", &NONE)?;
            supply(self)?;
            self.flush()?;
            let [status, byte, affinity, line, x, top, _, height, other_x] = self.best;
            if status != 0 {
                return Err(Error::Invalid);
            }
            Ok(CaretStop {
                byte: byte as usize,
                affinity: if affinity == 0 {
                    Affinity::Upstream
                } else {
                    Affinity::Downstream
                },
                line: line as usize,
                x: f32::from_bits(x),
                other_x: f32::from_bits(other_x),
                top: f32::from_bits(top),
                bottom: f32::from_bits(top) + f32::from_bits(height),
            })
        }));
        match result {
            Ok(Ok(hit)) => {
                self.decisions = self.decisions.saturating_add(1);
                Ok(hit)
            }
            Ok(Err(error @ Error::Language(_))) => {
                self.faulted = true;
                Err(error)
            }
            Ok(Err(error)) => Err(error),
            Err(_) => {
                self.faulted = true;
                Err(Error::Language("Hit-testing VM panicked".into()))
            }
        }
    }

    fn push(&mut self, stop: CaretStop) -> Result<(), Error> {
        if self.stop_count >= 2 * easl_native_text::MAX_TEXT_BYTES + 2 {
            return Err(Error::Limit);
        }
        self.stop_count += 1;
        if [stop.x, stop.other_x, stop.top, stop.bottom]
            .iter()
            .any(|v| !v.is_finite() || v.abs() > 1e30)
            || stop.bottom <= stop.top
            || stop.bottom - stop.top > 1e30
        {
            return Err(Error::Invalid);
        }
        self.words.extend([
            0,
            u32::try_from(stop.byte).map_err(|_| Error::Invalid)?,
            u32::from(stop.affinity == Affinity::Downstream),
            u32::try_from(stop.line).map_err(|_| Error::Invalid)?,
            stop.x.to_bits(),
            stop.top.to_bits(),
            1_f32.to_bits(),
            (stop.bottom - stop.top).to_bits(),
            stop.other_x.to_bits(),
        ]);
        if self.words.len() == BATCH * WORDS {
            self.flush()?;
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<(), Error> {
        if self.words.is_empty() {
            return Ok(());
        }
        self.runtime.write("text-hit-stops", &self.words)?;
        self.runtime.run("text-hit-native")?;
        let output = self.runtime.read("text-hit-best")?;
        // The policy selects an existing stop. Reject invented bytes/geometry
        // before returning a target to a mutable widget.
        if output != self.best && !self.words.chunks_exact(WORDS).any(|stop| stop == output) {
            return Err(Error::Language(
                "Hit tester returned an unknown caret".into(),
            ));
        }
        self.best.copy_from_slice(&output);
        self.words.clear();
        Ok(())
    }
}
