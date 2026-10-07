//! What a solve returns: the solved values and the diagnosis, as plain
//! data keyed by entity and constraint ids.

use std::fmt;

use crate::equations::{Coordinate, Source};
use crate::model::{ConstraintId, EntityId};

/// Whether the solve met every equation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// Every residual is within tolerance (see [`Solution::tolerance`]).
    /// The sketch may still have free degrees of freedom
    /// ([`Solution::dof`]) or redundant constraints.
    Solved,
    /// Some equation is not met. [`Solution::conflicts`] names the
    /// constraints that cannot hold together, when the failure is a
    /// conflict rather than a drawing too far from any solution.
    NotConverged,
}

/// A coordinate the constraints leave free to move.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FreeCoordinate {
    pub entity: EntityId,
    pub coordinate: Coordinate,
    /// How freely it moves, from 0 (pinned) to 1 (moves on its own): the
    /// length of the projection of this coordinate's direction onto the
    /// null space of the Jacobian. A point that can only slide along a
    /// 45° line has 0.71 in x and in y.
    pub mobility: f64,
}

/// An equation that depends on earlier ones: implied by them (redundant)
/// or contradicting them (a conflict).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dependency {
    /// The later equation's source.
    pub source: Source,
    /// The earlier equations' sources it is a combination of, in order.
    pub with: Vec<Source>,
}

/// A side or direction of the solution that differs from the drawing's,
/// which usually means the solver jumped to another branch (a "flip").
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Orientation {
    /// The arc sweeps more than 180° where it was drawn sweeping less, or
    /// the reverse.
    ArcSweep(EntityId),
    /// The angle constraint is met 180° from its value (the equation
    /// alone cannot tell them apart).
    AngleBranch(ConstraintId),
    /// The circle's radius came out negative.
    RadiusSign(EntityId),
    /// A tangency at a shared endpoint came out on the other side: the
    /// arc's centre across the line, or two arcs touching inside where
    /// they were drawn touching outside (or the reverse).
    TangentSide(ConstraintId),
    /// The two lines meeting at `point` turn the other way.
    Corner {
        point: EntityId,
        lines: [EntityId; 2],
    },
}

/// The result of [`crate::Sketch::solve`].
#[derive(Clone, Debug)]
pub struct Solution {
    pub status: Status,
    /// Unknowns: two per point, one per circle.
    pub unknowns: usize,
    /// Equations: from constraints, plus one per arc.
    pub equations: usize,
    /// The rank of the Jacobian at the solution.
    pub rank: usize,
    /// Free degrees of freedom: unknowns minus rank. A sketch with nothing
    /// fixed keeps at least 3 (it can move and turn as a whole), which is
    /// not an error: it stays where it was drawn.
    pub dof: usize,
    /// The coordinates that can still move, in unknown order.
    pub free: Vec<FreeCoordinate>,
    /// Equations implied by earlier ones.
    pub redundant: Vec<Dependency>,
    /// Equations that contradict earlier ones.
    pub conflicts: Vec<Dependency>,
    /// Where the solution's orientation differs from the drawing's.
    pub flipped: Vec<Orientation>,
    /// Points and circles that had no guess, in entity order: the solver
    /// placed them before solving.
    pub placed: Vec<EntityId>,
    /// Levenberg–Marquardt iterations, all phases together.
    pub iterations: u32,
    /// The largest residual at the solution, in sketch units.
    pub residual: f64,
    /// The residual tolerance: 1e-10 times [`Solution::size`].
    pub tolerance: f64,
    /// The sketch's size: the larger of the drawing's diagonal and its
    /// largest length dimension.
    pub size: f64,
    /// Whether continuation ran (the direct solve failed or flipped).
    pub continuation: bool,
    pub(crate) values: Vec<f64>,
    pub(crate) slots: Vec<Slot>,
}

/// Where an entity's values are among the unknowns.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Slot {
    Point(usize),
    Circle(usize),
    Arc { c: usize, s: usize },
    Line,
}

impl Solution {
    /// A point's solved position.
    pub fn point(&self, id: EntityId) -> Option<[f64; 2]> {
        match self.slots.get(id.index())? {
            Slot::Point(v) => Some([self.values[*v], self.values[*v + 1]]),
            _ => None,
        }
    }

    /// An arc's or circle's solved radius.
    pub fn radius(&self, id: EntityId) -> Option<f64> {
        match *self.slots.get(id.index())? {
            Slot::Circle(v) => Some(self.values[v]),
            Slot::Arc { c, s } => {
                let dx = self.values[s] - self.values[c];
                let dy = self.values[s + 1] - self.values[c + 1];
                Some((dx * dx + dy * dy).sqrt())
            }
            _ => None,
        }
    }

    /// Every unknown's value, in unknown order (each point's x then y, and
    /// each circle's radius, in entity order).
    pub fn values(&self) -> &[f64] {
        &self.values
    }

    /// Whether the sketch is solved with no free degree of freedom.
    pub fn is_fully_constrained(&self) -> bool {
        self.status == Status::Solved && self.dof == 0
    }
}

/// Why a solve did not run to the end.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SolveError {
    /// More unknowns than [`crate::SolveOptions::max_unknowns`].
    TooManyUnknowns { unknowns: usize, limit: usize },
    /// The interrupt callback asked to stop.
    Interrupted,
}

impl fmt::Display for SolveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SolveError::TooManyUnknowns { unknowns, limit } => {
                write!(
                    f,
                    "the sketch has {unknowns} unknowns, more than the limit of {limit}"
                )
            }
            SolveError::Interrupted => f.write_str("the solve was interrupted"),
        }
    }
}

impl std::error::Error for SolveError {}
