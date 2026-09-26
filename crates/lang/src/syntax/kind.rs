//! Token and node kinds of the concrete syntax tree.

/// Every token and node kind. Tokens sort before [`SyntaxKind::SourceFile`],
/// so `is_token` is one comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum SyntaxKind {
    // --- trivia -------------------------------------------------------
    /// Spaces, tabs, `\r`, `\n`, U+00A0, U+FEFF and a lone Latin-1 `0xA0`.
    Whitespace,
    LineComment,
    BlockComment,
    /// `include <path>`. OpenSCAD splices the included file's tokens into the
    /// stream at this point, so to the parser the directive itself is
    /// invisible.
    IncludeDirective,
    /// A float literal outside the range of a double (`1e400`, `1e-400`).
    /// OpenSCAD's scanner rule for it fails to return a token, which makes
    /// the text vanish from the token stream; keeping it as trivia
    /// reproduces that while staying lossless.
    DroppedNumber,
    /// Everything from a NUL byte to the end of the file. OpenSCAD reads
    /// its input as a C string, so a NUL ends the program text.
    Ignored,

    // --- significant tokens -------------------------------------------
    /// `use <path>`; only valid between top-level statements.
    UseDirective,
    Ident,
    Number,
    String,

    KwModule,
    KwFunction,
    KwIf,
    KwElse,
    KwFor,
    KwLet,
    KwAssert,
    KwEcho,
    KwEach,
    KwTrue,
    KwFalse,
    KwUndef,

    Semi,
    LBrace,
    RBrace,
    LParen,
    RParen,
    LBrack,
    RBrack,
    Comma,
    Eq,
    Bang,
    Hash,
    Percent,
    Star,
    Plus,
    Minus,
    Slash,
    Lt,
    Gt,
    Question,
    Colon,
    Dot,
    Caret,
    Amp,
    Pipe,
    Tilde,
    Le,
    Ge,
    EqEq,
    Ne,
    AndAnd,
    OrOr,
    Shl,
    Shr,
    /// Byte `0x03`. OpenSCAD appends `"\n\x03\n"` and then the `-D`
    /// assignments to the program text; the grammar accepts it as a
    /// statement that marks the end of the file proper.
    Eot,
    /// A byte sequence no rule accepts, or an unterminated string, comment
    /// or directive. Always a syntax error.
    Error,
    /// End of input; never stored in a tree.
    Eof,

    // --- nodes ----------------------------------------------------------
    SourceFile,
    /// Tokens skipped while recovering from a syntax error.
    ErrorNode,
    UseStmt,
    EmptyStmt,
    BlockStmt,
    Assignment,
    ModuleDef,
    FunctionDef,
    EotStmt,
    /// `name(args) child`.
    ModuleInst,
    /// `!inst`, `#inst`, `%inst` or `*inst`.
    ModifierInst,
    /// `if (cond) child [else child]`.
    IfInst,
    ElseClause,
    /// `{ ... }` after a module instantiation.
    ChildBlock,
    ParamList,
    Param,
    ArgList,
    Arg,

    Literal,
    NameRef,
    ParenExpr,
    UnaryExpr,
    BinaryExpr,
    TernaryExpr,
    CallExpr,
    IndexExpr,
    MemberExpr,
    RangeExpr,
    VectorExpr,
    FunctionExpr,
    LetExpr,
    AssertExpr,
    EchoExpr,
    LcLet,
    LcEach,
    LcFor,
    LcForC,
    LcIf,
    /// `( list-comprehension )`.
    LcParen,

    /// Placeholder for a node whose kind is not decided yet.
    Tombstone,
}

impl SyntaxKind {
    pub fn is_token(self) -> bool {
        self < SyntaxKind::SourceFile
    }

    pub fn is_node(self) -> bool {
        !self.is_token()
    }

    /// Tokens the parser never sees.
    pub fn is_trivia(self) -> bool {
        self <= SyntaxKind::Ignored
    }

    /// The keyword for an identifier's text, if it is one.
    pub fn keyword(text: &[u8]) -> Option<SyntaxKind> {
        Some(match text {
            b"module" => SyntaxKind::KwModule,
            b"function" => SyntaxKind::KwFunction,
            b"if" => SyntaxKind::KwIf,
            b"else" => SyntaxKind::KwElse,
            b"for" => SyntaxKind::KwFor,
            b"let" => SyntaxKind::KwLet,
            b"assert" => SyntaxKind::KwAssert,
            b"echo" => SyntaxKind::KwEcho,
            b"each" => SyntaxKind::KwEach,
            b"true" => SyntaxKind::KwTrue,
            b"false" => SyntaxKind::KwFalse,
            b"undef" => SyntaxKind::KwUndef,
            _ => return None,
        })
    }

    /// Can this token start an expression? (FIRST(expr) in parser.y.)
    pub fn starts_expr(self) -> bool {
        use SyntaxKind::*;
        matches!(
            self,
            KwFunction
                | KwLet
                | KwAssert
                | KwEcho
                | Bang
                | Plus
                | Minus
                | Tilde
                | KwTrue
                | KwFalse
                | KwUndef
                | Number
                | String
                | Ident
                | LParen
                | LBrack
        )
    }
}
