//! Bounded SVG geometry for native controls. No images, scripts, resource loads,
//! CSS or transforms: unsupported SVG input fails instead of rendering partly.
use crate::Error;
use vello_cpu::kurbo::{BezPath, Circle, Rect, RoundedRect, Shape};

#[derive(Debug)]
pub struct VectorIcon {
    pub(crate) viewbox: [f64; 4],
    pub(crate) paths: Vec<BezPath>,
}
impl VectorIcon {
    pub fn parse(svg: &str) -> Result<Self, Error> {
        if svg.len() > 8192 {
            return Err(Error::Limit);
        }
        let doc = roxmltree::Document::parse(svg).map_err(|_| Error::InvalidGeometry)?;
        let root = doc.root_element();
        if !root.has_tag_name("svg") {
            return Err(Error::InvalidGeometry);
        }
        attributes(root, &["viewBox", "aria-hidden"])?;
        let numbers = root
            .attribute("viewBox")
            .ok_or(Error::InvalidGeometry)?
            .split_whitespace()
            .map(number)
            .collect::<Result<Vec<_>, _>>()?;
        let viewbox: [f64; 4] = numbers.try_into().map_err(|_| Error::InvalidGeometry)?;
        if viewbox[2] <= 0. || viewbox[3] <= 0. {
            return Err(Error::InvalidGeometry);
        }
        let mut paths = Vec::new();
        for node in root.children().filter(roxmltree::Node::is_element) {
            if paths.len() >= 16 || node.attribute("transform").is_some() {
                return Err(Error::Limit);
            }
            let attr = |name| number(node.attribute(name).ok_or(Error::InvalidGeometry)?);
            attributes(
                node,
                match node.tag_name().name() {
                    "path" => &["d"],
                    "circle" => &["cx", "cy", "r"],
                    "rect" => &["x", "y", "width", "height", "rx"],
                    _ => return Err(Error::InvalidGeometry),
                },
            )?;
            let path = match node.tag_name().name() {
                "path" => BezPath::from_svg(node.attribute("d").ok_or(Error::InvalidGeometry)?)
                    .map_err(|_| Error::InvalidGeometry)?,
                "circle" => {
                    let radius = attr("r")?;
                    if radius < 0. {
                        return Err(Error::InvalidGeometry);
                    }
                    Circle::new((attr("cx")?, attr("cy")?), radius).to_path(0.01)
                }
                "rect" => {
                    let (x, y, w, h) = (attr("x")?, attr("y")?, attr("width")?, attr("height")?);
                    if w < 0. || h < 0. {
                        return Err(Error::InvalidGeometry);
                    }
                    let radius = node.attribute("rx").map_or(Ok(0.), number)?;
                    if radius < 0. {
                        return Err(Error::InvalidGeometry);
                    }
                    RoundedRect::from_rect(Rect::new(x, y, x + w, y + h), radius).to_path(0.01)
                }
                _ => return Err(Error::InvalidGeometry),
            };
            if path.elements().len() > 512 || !path.is_finite() {
                return Err(Error::Limit);
            }
            paths.push(path);
        }
        Ok(Self { viewbox, paths })
    }
}
fn attributes(node: roxmltree::Node<'_, '_>, allowed: &[&str]) -> Result<(), Error> {
    if node.attributes().any(|a| !allowed.contains(&a.name())) {
        return Err(Error::InvalidGeometry);
    }
    Ok(())
}
fn number(value: &str) -> Result<f64, Error> {
    let number: f64 = value.parse().map_err(|_| Error::InvalidGeometry)?;
    if !number.is_finite() || number.abs() > 4096. {
        return Err(Error::InvalidGeometry);
    }
    Ok(number)
}
