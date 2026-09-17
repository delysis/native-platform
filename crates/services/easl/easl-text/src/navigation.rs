//! Native geometry transport for reusable EASL line/page navigation policy.
use crate::{Error, HitTesting, runtime::Runtime};
use easl_native_text::{Movement, TextEditor, TextSystem, parley::Affinity};

#[derive(Clone, Copy, Debug)]
#[repr(u32)]
pub enum LineMovement {
    Up,
    Down,
    PageUp,
    PageDown,
    LineStart,
    LineEnd,
    TextStart,
    TextEnd,
}
impl LineMovement {
    pub fn from_movement(movement: Movement) -> Option<Self> {
        Some(match movement {
            Movement::Up => Self::Up,
            Movement::Down => Self::Down,
            Movement::PageUp => Self::PageUp,
            Movement::PageDown => Self::PageDown,
            Movement::LineStart => Self::LineStart,
            Movement::LineEnd => Self::LineEnd,
            Movement::TextStart => Self::TextStart,
            Movement::TextEnd => Self::TextEnd,
            _ => return None,
        })
    }
}

#[derive(Debug)]
struct Query {
    kind: u32,
    line: usize,
    x: f32,
    y: f32,
    edge: u32,
    preserve_x: bool,
}

pub struct Navigation {
    runtime: Runtime,
    faulted: bool,
    pub decisions: u64,
}
impl std::fmt::Debug for Navigation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Navigation")
            .field("faulted", &self.faulted)
            .field("decisions", &self.decisions)
            .finish_non_exhaustive()
    }
}
impl Navigation {
    pub fn new() -> Result<Self, Error> {
        let runtime = std::panic::catch_unwind(|| {
            Runtime::compile(&[
                (
                    "navigation-native.easl",
                    include_str!("../library/navigation-native.easl"),
                ),
                (
                    "navigation.easl",
                    include_str!("../library/navigation.easl"),
                ),
            ])
        })
        .map_err(|_| Error::Language("Navigation compiler panicked".into()))??;
        Ok(Self {
            runtime,
            faulted: false,
            decisions: 0,
        })
    }

    /// Apply a view-only movement through the shared EASL policy and hit tester.
    /// Each operation queries the live layout. No text, source transaction or
    /// history enters either VM, and failures leave selection unchanged.
    pub fn apply(
        &mut self,
        editor: &mut TextEditor,
        system: &mut TextSystem,
        hits: &mut HitTesting,
        movement: LineMovement,
        extend: bool,
    ) -> Result<(), Error> {
        if self.faulted {
            return Err(Error::Faulted);
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.apply_inner(editor, system, hits, movement, extend)
        }));
        match result {
            Ok(Ok(())) => {
                self.decisions = self.decisions.saturating_add(1);
                Ok(())
            }
            Ok(Err(error @ Error::Language(_))) => {
                self.faulted = true;
                Err(error)
            }
            Ok(Err(error)) => Err(error),
            Err(_) => {
                self.faulted = true;
                Err(Error::Language("Navigation VM panicked".into()))
            }
        }
    }

    fn apply_inner(
        &mut self,
        editor: &mut TextEditor,
        system: &mut TextSystem,
        hits: &mut HitTesting,
        movement: LineMovement,
        extend: bool,
    ) -> Result<(), Error> {
        let context = editor.navigation_context(system)?;
        self.runtime.write(
            "text-navigation-context",
            &[
                word(context.line)?,
                word(context.lines)?,
                context.x.to_bits(),
                context.top.to_bits(),
                (context.bottom - context.top).to_bits(),
                context.viewport_height.to_bits(),
                context.extent_height.to_bits(),
                context.preferred_x.unwrap_or(0.).to_bits(),
                u32::from(context.preferred_x.is_some()),
            ],
        )?;
        self.runtime
            .write("text-navigation-command", &[movement as u32])?;
        self.runtime.run("text-navigation-native")?;
        let mut query = self.query(context.lines)?;
        if query.kind == 1 {
            let found = editor.line_at_y(system, query.y)?;
            self.runtime
                .write("text-navigation-found", &[word(found)?])?;
            self.runtime.run("text-navigation-resolve-native")?;
            query = self.query(context.lines)?;
            if query.kind != 0 {
                return Err(Error::Invalid);
            }
        }
        let (byte, affinity) = if query.edge == 0 {
            let hit = hits.hit_line(editor, system, query.line, query.x)?;
            (hit.byte, hit.affinity)
        } else {
            let line = editor.line_geometry(system, query.line)?;
            self.runtime.write(
                "text-navigation-line",
                &[
                    word(line.start)?,
                    word(line.end)?,
                    word(line.separator_start)?,
                ],
            )?;
            self.runtime.run("text-navigation-edge-native")?;
            let output = self.runtime.read("text-navigation-target")?;
            let [0, byte, affinity] = output.as_slice() else {
                return Err(Error::Invalid);
            };
            let byte = *byte as usize;
            if *affinity > 1 || ![line.start, line.end, line.separator_start].contains(&byte) {
                return Err(Error::Invalid);
            }
            (
                byte,
                if *affinity == 0 {
                    Affinity::Upstream
                } else {
                    Affinity::Downstream
                },
            )
        };
        editor.select_navigation_target(
            system,
            byte,
            affinity,
            extend,
            query.preserve_x.then_some(query.x),
        )?;
        Ok(())
    }

    fn query(&self, lines: usize) -> Result<Query, Error> {
        let output = self.runtime.read("text-navigation-query")?;
        let [0, kind, line, x, y, edge, preserve] = output.as_slice() else {
            return Err(Error::Invalid);
        };
        let (x, y) = (f32::from_bits(*x), f32::from_bits(*y));
        if *kind > 1
            || *line as usize >= lines
            || *edge > 3
            || *preserve > 1
            || [x, y].iter().any(|v| !v.is_finite() || v.abs() > 1e30)
        {
            return Err(Error::Invalid);
        }
        Ok(Query {
            kind: *kind,
            line: *line as usize,
            x,
            y,
            edge: *edge,
            preserve_x: *preserve == 1,
        })
    }
}
fn word(value: usize) -> Result<u32, Error> {
    u32::try_from(value).map_err(|_| Error::Limit)
}
