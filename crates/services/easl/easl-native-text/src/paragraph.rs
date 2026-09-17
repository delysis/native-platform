//! Bounded minimum-raggedness paragraph breaking over already shaped clusters.
//! This is a global paragraph optimizer, not automatic hyphenation or TeX math.
use crate::{Alignment, Error, LineStats, PreparedText};
use parley::AlignmentOptions;

#[derive(Debug, Clone, Copy)]
pub struct BreakOpportunity {
    /// Logical cluster boundary; not a UTF-8 byte offset or a glyph index.
    pub cluster: usize,
    /// Cumulative logical advance in pixels, including trailing whitespace.
    pub advance: f64,
    /// Cumulative advance with the current trailing whitespace removed.
    pub trimmed: f64,
    /// Additional shaped advance when this discretionary break is selected.
    pub hyphen: f64,
    /// A hard paragraph boundary that every composition must retain.
    pub mandatory: bool,
}

impl PreparedText {
    /// Minimize squared raggedness across each paragraph, with penalties for
    /// discretionary and consecutive hyphens. Font shaping is reused.
    ///
    /// At most 4096 candidate breaks and one million edges are examined. Tabs,
    /// inline boxes, negative advances and unbreakable overwide words return an
    /// error with the prior layout intact; `reflow` remains available for those.
    /// This method does not insert hyphens into the source.
    pub fn optimize(&mut self, width: f32, alignment: Alignment) -> Result<LineStats, Error> {
        self.compose_with(width, alignment, |points, width| {
            solve(points, f64::from(width))
        })
    }

    /// Let a language library choose lines over native shaped measurements.
    /// The callback returns logical cluster boundaries. Every returned boundary,
    /// mandatory break and line width is validated before applying the choice.
    /// Failed composition retains the previous drawable layout and source.
    pub fn compose_with<E: From<Error>>(
        &mut self,
        width: f32,
        alignment: Alignment,
        choose: impl FnOnce(&[BreakOpportunity], f32) -> Result<Vec<usize>, E>,
    ) -> Result<LineStats, E> {
        if !width.is_finite() || width <= 0. || width > 1e7 {
            return Err(Error::InvalidGeometry.into());
        }
        if !self.layout.inline_boxes().is_empty() || self.normalized_text().contains('\t') {
            return Err(Error::UnsupportedFlow.into());
        }
        let previous = self.layout.clone();
        let result = (|| {
            let points = self.break_opportunities()?;
            if self.normalized_text().is_empty() {
                return Ok(self.stats());
            }
            let breaks = choose(&points, width)?;
            validate_breaks(&points, &breaks, f64::from(width))?;
            self.apply_breaks(width, alignment, breaks).map_err(E::from)
        })();
        if result.is_err() {
            self.layout = previous;
        }
        result
    }

    fn break_opportunities(&mut self) -> Result<Vec<BreakOpportunity>, Error> {
        self.layout.break_all_lines(None);
        let mut clusters = Vec::new();
        for line in self.layout.lines() {
            for run in line.runs() {
                for cluster in run.clusters() {
                    let range = cluster.text_range();
                    let ch = self
                        .normalized_text()
                        .get(range.clone())
                        .and_then(|s| s.chars().next())
                        .ok_or(Error::InvalidRange)?;
                    clusters.push((
                        range.start,
                        cluster.advance(),
                        cluster.can_break_before(),
                        cluster.is_hard_line_break(),
                        ch,
                        cluster.discretionary_advance(),
                        cluster.allows_wrapping(),
                    ));
                }
            }
        }
        clusters.sort_by_key(|c| c.0);
        let mut points = vec![BreakOpportunity {
            cluster: 0,
            advance: 0.,
            trimmed: 0.,
            hyphen: 0.,
            mandatory: true,
        }];
        let mut advance = 0.;
        let mut trailing = 0.;
        let mut wraps = true;
        for (i, (_, cluster_advance, can_break, newline, ch, hyphen, wrap)) in
            clusters.iter().enumerate()
        {
            if !cluster_advance.is_finite() || *cluster_advance < 0. {
                return Err(Error::UnsupportedFlow);
            }
            if *can_break && wraps && points.last().is_some_and(|p| p.cluster != i) {
                points.push(BreakOpportunity {
                    cluster: i,
                    advance,
                    trimmed: advance - trailing,
                    hyphen: if i > 0 {
                        f64::from(clusters[i - 1].5)
                    } else {
                        0.
                    },
                    mandatory: false,
                });
            }
            advance += f64::from(*cluster_advance);
            trailing = if matches!(ch, ' ' | '\r' | '\n' | '\u{c}' | '\u{2028}' | '\u{2029}') {
                trailing + f64::from(*cluster_advance)
            } else {
                0.
            };
            wraps = *wrap;
            // CRLF is one hard break even when the font produced separate runs.
            let crlf_prefix = *ch == '\r' && clusters.get(i + 1).is_some_and(|c| c.4 == '\n');
            if (*newline && !crlf_prefix) || i + 1 == clusters.len() {
                points.push(BreakOpportunity {
                    cluster: i + 1,
                    advance,
                    trimmed: advance - trailing,
                    hyphen: if *newline { 0. } else { f64::from(*hyphen) },
                    mandatory: true,
                });
            }
            if points.len() > 4096 {
                return Err(Error::Limit);
            }
        }
        Ok(points)
    }

    fn apply_breaks(
        &mut self,
        width: f32,
        alignment: Alignment,
        breaks: Vec<usize>,
    ) -> Result<LineStats, Error> {
        let mut breaker = self.layout.break_lines();
        breaker.state_mut().set_layout_max_advance(width);
        breaker.state_mut().set_line_max_advance(width);
        let mut start = 0;
        for end in breaks {
            let count = u32::try_from(end - start).map_err(|_| Error::Limit)?;
            if count == 0 || breaker.break_next_with_length(count).is_none() {
                return Err(Error::UnsupportedFlow);
            }
            start = end;
        }
        // A final paragraph separator owns an additional empty visual line.
        if !breaker.is_done() {
            breaker.break_next();
        }
        if !breaker.is_done() {
            return Err(Error::UnsupportedFlow);
        }
        breaker.finish();
        self.layout
            .align(alignment.into(), AlignmentOptions::default());
        self.check_geometry()?;
        self.reflows = self.reflows.saturating_add(1);
        Ok(self.stats())
    }
}

fn validate_breaks(points: &[BreakOpportunity], breaks: &[usize], width: f64) -> Result<(), Error> {
    let mut start = 0;
    for &cluster in breaks {
        let end = points
            .binary_search_by_key(&cluster, |point| point.cluster)
            .map_err(|_| Error::InvalidRange)?;
        if end <= start || points[start + 1..end].iter().any(|point| point.mandatory) {
            return Err(Error::InvalidRange);
        }
        let point = points[end];
        let hyphen = if point.mandatory { 0. } else { point.hyphen };
        if point.trimmed - points[start].advance + hyphen > width + 0.01 {
            return Err(Error::UnsupportedFlow);
        }
        start = end;
    }
    if start + 1 != points.len() {
        return Err(Error::InvalidRange);
    }
    Ok(())
}

fn solve(points: &[BreakOpportunity], width: f64) -> Result<Vec<usize>, Error> {
    let mut cost = vec![f64::INFINITY; points.len()];
    let mut previous = vec![0; points.len()];
    cost[0] = 0.;
    let mut paragraph_start = 0;
    let mut edges = 0usize;
    for end in 1..points.len() {
        let point = points[end];
        for start in (paragraph_start..end).rev() {
            edges += 1;
            if edges > 1_000_000 {
                return Err(Error::Limit);
            }
            let hyphen = if point.mandatory { 0. } else { point.hyphen };
            let occupied = point.trimmed - points[start].advance + hyphen;
            if occupied > width + 0.01 {
                break;
            }
            if !cost[start].is_finite() {
                continue;
            }
            let slack = (width - occupied).max(0.);
            let penalty = if hyphen != 0. {
                width
                    * width
                    * (if points[start].hyphen != 0. {
                        0.2
                    } else {
                        0.05
                    })
            } else {
                0.
            };
            let candidate =
                cost[start] + if point.mandatory { 0. } else { slack * slack } + penalty;
            if candidate < cost[end] {
                cost[end] = candidate;
                previous[end] = start;
            }
        }
        if point.mandatory {
            paragraph_start = end;
        }
    }
    if !cost.last().is_some_and(|cost| cost.is_finite()) {
        return Err(Error::UnsupportedFlow);
    }
    let mut result = Vec::new();
    let mut index = points.len() - 1;
    while index > 0 {
        result.push(points[index].cluster);
        index = previous[index];
    }
    result.reverse();
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn global_breaking_avoids_the_greedy_short_middle_line() {
        // Words of widths 3, 2, 2, 5 with one-unit spaces and a six-unit measure.
        // Greedy gives [3+1+2], [2], [5]. Global gives [3], [2+1+2], [5].
        let points = [
            (0, 0., 0.),
            (4, 4., 3.),
            (7, 7., 6.),
            (10, 10., 9.),
            (15, 15., 15.),
        ]
        .into_iter()
        .enumerate()
        .map(|(i, (cluster, advance, trimmed))| BreakOpportunity {
            cluster,
            advance,
            trimmed,
            hyphen: 0.,
            mandatory: i == 0 || i == 4,
        })
        .collect::<Vec<_>>();
        assert_eq!(solve(&points, 6.).unwrap(), vec![4, 10, 15]);
    }
}
