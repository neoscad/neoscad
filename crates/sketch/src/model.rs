//! The sketch: entities, constraints and the checks made when they are
//! added.

use std::fmt;

/// An entity in a [`Sketch`]: a point, line, arc or circle. Ids are
/// numbered in creation order from 0, across all kinds, so a caller can
/// keep its own table (labels, source locations) indexed by
/// [`EntityId::index`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EntityId(u32);

impl EntityId {
    /// The creation index of the entity.
    pub fn index(self) -> usize {
        self.0 as usize
    }

    pub(crate) fn from_index(i: usize) -> EntityId {
        EntityId(i as u32)
    }
}

/// A constraint in a [`Sketch`], numbered in the order constraints were
/// added, from 0.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ConstraintId(u32);

impl ConstraintId {
    /// The index of the constraint in the order it was added.
    pub fn index(self) -> usize {
        self.0 as usize
    }

    pub(crate) fn from_index(i: usize) -> ConstraintId {
        ConstraintId(i as u32)
    }
}

/// What an entity is, and what it is made of.
#[derive(Clone, Debug, PartialEq)]
pub enum Entity {
    /// A free point. `guess` is where it is drawn; the solver starts from
    /// there. Without one, the solver places it (see [`crate::Solution::placed`]).
    Point { guess: Option<[f64; 2]> },
    /// A segment between two points.
    Line { start: EntityId, end: EntityId },
    /// A circular arc around `center`, counter-clockwise from `start` to
    /// `end` (clockwise when `clockwise`). It adds one equation of its own:
    /// `start` and `end` are the same distance from `center`.
    Arc {
        center: EntityId,
        start: EntityId,
        end: EntityId,
        clockwise: bool,
    },
    /// A full circle. Its radius is an unknown; `radius_guess` is where the
    /// solver starts it.
    Circle {
        center: EntityId,
        radius_guess: Option<f64>,
    },
}

/// The kind of an [`Entity`], for messages.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EntityKind {
    Point,
    Line,
    Arc,
    Circle,
}

impl fmt::Display for EntityKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            EntityKind::Point => "point",
            EntityKind::Line => "line",
            EntityKind::Arc => "arc",
            EntityKind::Circle => "circle",
        })
    }
}

impl Entity {
    /// Its kind.
    pub fn kind(&self) -> EntityKind {
        match self {
            Entity::Point { .. } => EntityKind::Point,
            Entity::Line { .. } => EntityKind::Line,
            Entity::Arc { .. } => EntityKind::Arc,
            Entity::Circle { .. } => EntityKind::Circle,
        }
    }
}

/// Two points that a horizontal or vertical constraint lines up: a line's
/// ends, or two points given directly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pair {
    Line(EntityId),
    Points(EntityId, EntityId),
}

/// How a point-to-point distance is measured.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Along {
    /// The Euclidean distance, which is never negative.
    #[default]
    Direct,
    /// The signed horizontal offset `b.x - a.x` (FreeCAD's DistanceX).
    X,
    /// The signed vertical offset `b.y - a.y` (FreeCAD's DistanceY).
    Y,
}

/// A constraint: one or more equations between entities.
///
/// Where an equation has two solutions that differ only in side or
/// direction (a point on either side of a line, a tangent circle inside or
/// outside another), the solver keeps the side the drawing (the guesses)
/// shows.
#[derive(Clone, Debug, PartialEq)]
pub enum Constraint {
    /// Two points are the same point (2 equations).
    Coincident(EntityId, EntityId),
    /// A point lies on a curve: on the infinite line through a line, or on
    /// an arc's or circle's circle (1 equation).
    On { point: EntityId, curve: EntityId },
    /// Δy = 0 between the two points (1 equation).
    Horizontal(Pair),
    /// Δx = 0 between the two points (1 equation).
    Vertical(Pair),
    /// Two lines are parallel, in either direction (1 equation).
    Parallel(EntityId, EntityId),
    /// Two lines are perpendicular (1 equation).
    Perpendicular(EntityId, EntityId),
    /// Line–arc/circle, arc/circle–arc/circle tangency (1 equation). The
    /// side, and inside or outside for two circles, come from the drawing.
    Tangent(EntityId, EntityId),
    /// A distance (1 equation): point–point ([`Along`] chooses Euclidean or
    /// a signed axis offset), point–line (to the infinite line, on the
    /// drawn side), or line–line (from the second line's start to the first
    /// line; the lines are meant to be parallel, which this does not
    /// impose).
    Distance {
        a: EntityId,
        b: EntityId,
        value: f64,
        along: Along,
    },
    /// The length of a line (1 equation).
    Length { line: EntityId, value: f64 },
    /// The radius of an arc or circle (1 equation).
    Radius { curve: EntityId, value: f64 },
    /// The diameter of an arc or circle (1 equation).
    Diameter { curve: EntityId, value: f64 },
    /// The signed angle, counter-clockwise in degrees, from the direction
    /// of line `from` to the direction of line `to` (1 equation).
    Angle {
        from: EntityId,
        to: EntityId,
        degrees: f64,
    },
    /// The angle an arc sweeps, in degrees, in its own direction
    /// (1 equation).
    Sweep { arc: EntityId, degrees: f64 },
    /// Equal lengths (two lines) or equal radii (arcs and circles)
    /// (1 equation).
    Equal(EntityId, EntityId),
    /// A point is the midpoint of a line (2 equations).
    Midpoint { point: EntityId, line: EntityId },
    /// `a` and `b` are mirror images about a line, or about a point
    /// (2 equations).
    Symmetric {
        a: EntityId,
        b: EntityId,
        about: EntityId,
    },
    /// A point stays at `at`, or at its guess when `at` is `None`
    /// (2 equations). A line fixes both its points (4 equations); `at` must
    /// then be `None`.
    Fix {
        entity: EntityId,
        at: Option<[f64; 2]>,
    },
}

/// Why an entity or constraint was not accepted.
#[derive(Clone, Debug, PartialEq)]
pub enum ModelError {
    /// The id does not name an entity of this sketch.
    UnknownEntity(EntityId),
    /// An argument has the wrong kind of entity. `argument` counts from 0.
    WrongKind {
        argument: usize,
        found: EntityKind,
        expected: &'static [EntityKind],
    },
    /// A value is not finite (NaN or infinite), or out of range (a
    /// negative radius, length or distance).
    BadValue { argument: usize, value: f64 },
    /// The same entity given twice where two different ones are needed
    /// (`Parallel(l, l)`, a line from a point to itself).
    SameEntity { argument: usize },
    /// `Fix` on a point without a guess and without `at`, or `at` on a
    /// line.
    NothingToFixAt,
}

impl fmt::Display for ModelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ModelError::UnknownEntity(e) => write!(f, "no entity {}", e.index()),
            ModelError::WrongKind {
                argument,
                found,
                expected,
            } => {
                write!(f, "argument {} is a {found}, expected ", argument + 1)?;
                for (i, k) in expected.iter().enumerate() {
                    if i > 0 {
                        f.write_str(if i + 1 == expected.len() {
                            " or "
                        } else {
                            ", "
                        })?;
                    }
                    write!(f, "{k}")?;
                }
                Ok(())
            }
            ModelError::BadValue { argument, value } => {
                write!(f, "argument {} has an invalid value {value}", argument + 1)
            }
            ModelError::SameEntity { argument } => {
                write!(f, "argument {} repeats an earlier argument", argument + 1)
            }
            ModelError::NothingToFixAt => f.write_str(
                "nothing to fix at: the point has no guess, or `at` was given for a line",
            ),
        }
    }
}

impl std::error::Error for ModelError {}

/// A 2D sketch: entities, construction flags and constraints, in the
/// order they were added. Solving does not change it ([`Sketch::solve`]).
#[derive(Clone, Debug, Default)]
pub struct Sketch {
    pub(crate) entities: Vec<Entity>,
    pub(crate) construction: Vec<bool>,
    pub(crate) constraints: Vec<Constraint>,
}

const POINT: &[EntityKind] = &[EntityKind::Point];
const LINE: &[EntityKind] = &[EntityKind::Line];
const ARC: &[EntityKind] = &[EntityKind::Arc];
const ROUND: &[EntityKind] = &[EntityKind::Arc, EntityKind::Circle];
const CURVE: &[EntityKind] = &[EntityKind::Line, EntityKind::Arc, EntityKind::Circle];
const POINT_OR_LINE: &[EntityKind] = &[EntityKind::Point, EntityKind::Line];

impl Sketch {
    /// An empty sketch.
    pub fn new() -> Sketch {
        Sketch::default()
    }

    /// The entities, by [`EntityId::index`].
    pub fn entities(&self) -> &[Entity] {
        &self.entities
    }

    /// The constraints, by [`ConstraintId::index`].
    pub fn constraints(&self) -> &[Constraint] {
        &self.constraints
    }

    /// The entity `id` names.
    pub fn entity(&self, id: EntityId) -> Option<&Entity> {
        self.entities.get(id.index())
    }

    /// Whether `id` is construction geometry. The solver treats it like
    /// any other entity; the flag is kept for whoever turns the solution
    /// into a profile.
    pub fn is_construction(&self, id: EntityId) -> bool {
        self.construction.get(id.index()).copied().unwrap_or(false)
    }

    /// Mark `id` as construction geometry, or not.
    pub fn set_construction(&mut self, id: EntityId, construction: bool) -> Result<(), ModelError> {
        self.kind(id)?;
        self.construction[id.index()] = construction;
        Ok(())
    }

    fn push(&mut self, e: Entity) -> EntityId {
        let id = EntityId(self.entities.len() as u32);
        self.entities.push(e);
        self.construction.push(false);
        id
    }

    fn kind(&self, id: EntityId) -> Result<EntityKind, ModelError> {
        self.entity(id)
            .map(Entity::kind)
            .ok_or(ModelError::UnknownEntity(id))
    }

    fn expect(
        &self,
        id: EntityId,
        argument: usize,
        expected: &'static [EntityKind],
    ) -> Result<EntityKind, ModelError> {
        let found = self.kind(id)?;
        if expected.contains(&found) {
            Ok(found)
        } else {
            Err(ModelError::WrongKind {
                argument,
                found,
                expected,
            })
        }
    }

    /// Add a point, drawn at `guess`.
    pub fn point(&mut self, guess: Option<[f64; 2]>) -> Result<EntityId, ModelError> {
        if let Some([x, y]) = guess {
            for (i, v) in [x, y].into_iter().enumerate() {
                if !v.is_finite() {
                    return Err(ModelError::BadValue {
                        argument: i,
                        value: v,
                    });
                }
            }
        }
        Ok(self.push(Entity::Point { guess }))
    }

    /// Add a line from `start` to `end`, two different points.
    pub fn line(&mut self, start: EntityId, end: EntityId) -> Result<EntityId, ModelError> {
        self.expect(start, 0, POINT)?;
        self.expect(end, 1, POINT)?;
        if start == end {
            return Err(ModelError::SameEntity { argument: 1 });
        }
        Ok(self.push(Entity::Line { start, end }))
    }

    /// Add an arc around `center` from `start` to `end`, counter-clockwise
    /// unless `clockwise`.
    pub fn arc(
        &mut self,
        center: EntityId,
        start: EntityId,
        end: EntityId,
        clockwise: bool,
    ) -> Result<EntityId, ModelError> {
        self.expect(center, 0, POINT)?;
        self.expect(start, 1, POINT)?;
        self.expect(end, 2, POINT)?;
        if start == center {
            return Err(ModelError::SameEntity { argument: 1 });
        }
        if end == center {
            return Err(ModelError::SameEntity { argument: 2 });
        }
        Ok(self.push(Entity::Arc {
            center,
            start,
            end,
            clockwise,
        }))
    }

    /// Add a circle around `center`, its radius starting at
    /// `radius_guess`.
    pub fn circle(
        &mut self,
        center: EntityId,
        radius_guess: Option<f64>,
    ) -> Result<EntityId, ModelError> {
        self.expect(center, 0, POINT)?;
        if let Some(r) = radius_guess
            && !(r.is_finite() && r >= 0.0)
        {
            return Err(ModelError::BadValue {
                argument: 1,
                value: r,
            });
        }
        Ok(self.push(Entity::Circle {
            center,
            radius_guess,
        }))
    }

    /// Add a constraint after checking its arguments.
    pub fn add(&mut self, c: Constraint) -> Result<ConstraintId, ModelError> {
        self.check(&c)?;
        let id = ConstraintId(self.constraints.len() as u32);
        self.constraints.push(c);
        Ok(id)
    }

    fn check(&self, c: &Constraint) -> Result<(), ModelError> {
        let finite = |argument: usize, value: f64| {
            if value.is_finite() {
                Ok(())
            } else {
                Err(ModelError::BadValue { argument, value })
            }
        };
        let nonneg = |argument: usize, value: f64| {
            if value.is_finite() && value >= 0.0 {
                Ok(())
            } else {
                Err(ModelError::BadValue { argument, value })
            }
        };
        let pair = |p: &Pair| -> Result<(), ModelError> {
            match *p {
                Pair::Line(l) => self.expect(l, 0, LINE).map(drop),
                Pair::Points(a, b) => {
                    self.expect(a, 0, POINT)?;
                    self.expect(b, 1, POINT)?;
                    if a == b {
                        return Err(ModelError::SameEntity { argument: 1 });
                    }
                    Ok(())
                }
            }
        };
        let distinct = |a: EntityId, b: EntityId| {
            if a == b {
                Err(ModelError::SameEntity { argument: 1 })
            } else {
                Ok(())
            }
        };
        match c {
            Constraint::Coincident(a, b) => {
                self.expect(*a, 0, POINT)?;
                self.expect(*b, 1, POINT)?;
                distinct(*a, *b)
            }
            Constraint::On { point, curve } => {
                self.expect(*point, 0, POINT)?;
                self.expect(*curve, 1, CURVE)?;
                Ok(())
            }
            Constraint::Horizontal(p) | Constraint::Vertical(p) => pair(p),
            Constraint::Parallel(a, b) | Constraint::Perpendicular(a, b) => {
                self.expect(*a, 0, LINE)?;
                self.expect(*b, 1, LINE)?;
                distinct(*a, *b)
            }
            Constraint::Tangent(a, b) => {
                let ka = self.expect(*a, 0, CURVE)?;
                let kb = self.expect(*b, 1, CURVE)?;
                if ka == EntityKind::Line && kb == EntityKind::Line {
                    return Err(ModelError::WrongKind {
                        argument: 1,
                        found: kb,
                        expected: ROUND,
                    });
                }
                distinct(*a, *b)
            }
            Constraint::Distance { a, b, value, along } => {
                let ka = self.expect(*a, 0, POINT_OR_LINE)?;
                let kb = self.expect(*b, 1, POINT_OR_LINE)?;
                match along {
                    Along::Direct => nonneg(2, *value)?,
                    Along::X | Along::Y => {
                        finite(2, *value)?;
                        self.expect(*a, 0, POINT)?;
                        self.expect(*b, 1, POINT)?;
                    }
                }
                if ka == EntityKind::Line && kb == EntityKind::Point {
                    // Point–line is written point first.
                    return Err(ModelError::WrongKind {
                        argument: 0,
                        found: ka,
                        expected: POINT,
                    });
                }
                distinct(*a, *b)
            }
            Constraint::Length { line, value } => {
                self.expect(*line, 0, LINE)?;
                nonneg(1, *value)
            }
            Constraint::Radius { curve, value } | Constraint::Diameter { curve, value } => {
                self.expect(*curve, 0, ROUND)?;
                nonneg(1, *value)
            }
            Constraint::Angle { from, to, degrees } => {
                self.expect(*from, 0, LINE)?;
                self.expect(*to, 1, LINE)?;
                distinct(*from, *to)?;
                finite(2, *degrees)
            }
            Constraint::Sweep { arc, degrees } => {
                self.expect(*arc, 0, ARC)?;
                finite(1, *degrees)
            }
            Constraint::Equal(a, b) => {
                let ka = self.expect(
                    *a,
                    0,
                    &[EntityKind::Line, EntityKind::Arc, EntityKind::Circle],
                )?;
                let want: &'static [EntityKind] = if ka == EntityKind::Line { LINE } else { ROUND };
                self.expect(*b, 1, want)?;
                distinct(*a, *b)
            }
            Constraint::Midpoint { point, line } => {
                self.expect(*point, 0, POINT)?;
                self.expect(*line, 1, LINE)?;
                Ok(())
            }
            Constraint::Symmetric { a, b, about } => {
                self.expect(*a, 0, POINT)?;
                self.expect(*b, 1, POINT)?;
                distinct(*a, *b)?;
                self.expect(*about, 2, POINT_OR_LINE)?;
                Ok(())
            }
            Constraint::Fix { entity, at } => match self.expect(*entity, 0, POINT_OR_LINE)? {
                EntityKind::Point => match (at, &self.entities[entity.index()]) {
                    (Some([x, y]), _) => {
                        finite(1, *x)?;
                        finite(1, *y)
                    }
                    (None, Entity::Point { guess: Some(_) }) => Ok(()),
                    _ => Err(ModelError::NothingToFixAt),
                },
                _ => {
                    if at.is_some() {
                        return Err(ModelError::NothingToFixAt);
                    }
                    let Entity::Line { start, end } = self.entities[entity.index()] else {
                        unreachable!("checked to be a line")
                    };
                    for p in [start, end] {
                        if !matches!(self.entities[p.index()], Entity::Point { guess: Some(_) }) {
                            return Err(ModelError::NothingToFixAt);
                        }
                    }
                    Ok(())
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_are_checked() {
        let mut s = Sketch::new();
        let a = s.point(Some([0.0, 0.0])).unwrap();
        let b = s.point(Some([1.0, 0.0])).unwrap();
        let l = s.line(a, b).unwrap();
        assert!(matches!(
            s.add(Constraint::Length {
                line: a,
                value: 1.0
            }),
            Err(ModelError::WrongKind { argument: 0, .. })
        ));
        assert!(matches!(
            s.add(Constraint::Length {
                line: l,
                value: -1.0
            }),
            Err(ModelError::BadValue { .. })
        ));
        assert!(s.line(a, a).is_err());
        let c = s.circle(a, None).unwrap();
        assert!(s.add(Constraint::Tangent(l, c)).is_ok());
        assert!(s.add(Constraint::Equal(l, c)).is_err());
        let free = s.point(None).unwrap();
        assert_eq!(
            s.add(Constraint::Fix {
                entity: free,
                at: None
            }),
            Err(ModelError::NothingToFixAt)
        );
        assert_eq!(s.constraints().len(), 1);
        assert_eq!(
            ModelError::WrongKind {
                argument: 0,
                found: EntityKind::Line,
                expected: ROUND
            }
            .to_string(),
            "argument 1 is a line, expected arc or circle"
        );
    }
}
