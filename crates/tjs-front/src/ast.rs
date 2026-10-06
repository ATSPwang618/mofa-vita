//! Nodes live in a flat arena: destruction cannot recurse through a huge tree.

use std::sync::Arc;
use tjs_core::Span;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExprId(pub(crate) usize);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StmtId(pub(crate) usize);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FunctionId(pub(crate) usize);

#[derive(Debug)]
pub struct Function {
    pub kind: FunctionKind,
    pub name: Option<Span>,
    pub parent: Option<FunctionId>,
    pub parameters: Vec<Parameter>,
    pub rest: Option<RestParameter>,
    pub body: StmtId,
    pub span: Span,
}

#[derive(Debug)]
pub enum FunctionKind {
    Function,
    Expression,
    Class {
        bases: Vec<ExprId>,
    },
    Property {
        getter: Option<FunctionId>,
        setter: Option<FunctionId>,
    },
    Accessor,
}

#[derive(Clone, Copy, Debug)]
pub enum Arguments {
    List { start: usize, end: usize },
    ForwardOriginal,
}

#[derive(Clone, Copy, Debug)]
pub enum Argument {
    Value(ExprId),
    Spread(ExprId),
    ForwardRest(Span),
}

#[derive(Clone, Copy, Debug)]
pub enum RestParameter {
    Named(Span),
    Unnamed,
}

#[derive(Clone, Copy, Debug)]
pub struct Parameter {
    pub name: Span,
    pub default: Option<ExprId>,
}

#[derive(Clone, Copy, Debug)]
pub struct Variable {
    pub name: Span,
    pub initializer: Option<ExprId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinaryOp {
    LogicalAnd,
    LogicalOr,
    In,
    InstanceOf,
    Add,
    Subtract,
    Multiply,
    Divide,
    IntDivide,
    Remainder,
    BitAnd,
    BitOr,
    BitXor,
    ShiftLeft,
    ShiftRight,
    ShiftRightUnsigned,
    Equal,
    NotEqual,
    StrictEqual,
    StrictNotEqual,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
}

#[derive(Clone, Copy, Debug)]
pub enum UnaryOp {
    CharacterCode,
    CharacterFrom,
    IsValid,
    Invalidate,
    Number,
    Integer,
    Real,
    String,
    BitNot,
}

#[derive(Clone, Copy, Debug)]
pub enum MemberName {
    Named(Span),
    Computed(ExprId),
}

#[derive(Clone, Copy, Debug)]
pub enum ExprKind {
    Swap {
        lhs: ExprId,
        rhs: ExprId,
    },
    PostfixIf {
        body: ExprId,
        condition: ExprId,
    },
    Void,
    Integer(i64),
    Real(u64),
    String(u32),
    Octet(u32),
    RegExp(u32),
    NameString(Span),
    Null,
    This,
    Super,
    Global,
    WithObject,
    Function(FunctionId),
    ConstantArray {
        start: usize,
        end: usize,
    },
    ConstantDictionary {
        start: usize,
        end: usize,
    },
    InContextOf {
        object: ExprId,
        context: ExprId,
    },
    Dictionary {
        start: usize,
        end: usize,
    },
    Array {
        start: usize,
        end: usize,
    },
    Name(Span),
    Negate(ExprId),
    Not(ExprId),
    Eval(ExprId),
    Unary {
        op: UnaryOp,
        inner: ExprId,
    },
    RawProperty(ExprId),
    Dereference(ExprId),
    TypeOf(ExprId),
    Delete(ExprId),
    Member {
        object: ExprId,
        name: MemberName,
    },
    Call {
        callee: ExprId,
        arguments: Arguments,
    },
    Construct {
        callee: ExprId,
        arguments: Arguments,
    },
    Binary {
        op: BinaryOp,
        lhs: ExprId,
        rhs: ExprId,
    },
    Logical {
        and: bool,
        lhs: ExprId,
        rhs: ExprId,
    },
    Conditional {
        condition: ExprId,
        then_value: ExprId,
        else_value: ExprId,
    },
    Sequence {
        lhs: ExprId,
        rhs: ExprId,
    },
    Assign {
        target: ExprId,
        value: ExprId,
    },
    CompoundAssign {
        target: ExprId,
        op: BinaryOp,
        value: ExprId,
    },
    Update {
        target: ExprId,
        increment: bool,
        postfix: bool,
    },
}

#[derive(Clone, Copy, Debug)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
    pub(crate) depth: usize,
}

#[derive(Debug)]
pub enum Statement {
    Try {
        body: StmtId,
        catch_name: Option<Span>,
        catch_body: StmtId,
        span: Span,
    },
    Throw {
        value: ExprId,
        span: Span,
    },
    Function {
        function: FunctionId,
        span: Span,
    },
    Return {
        value: Option<ExprId>,
        span: Span,
    },
    Var {
        variables: Vec<Variable>,
        span: Span,
    },
    Expression(ExprId),
    Block {
        statements: Vec<StmtId>,
        span: Span,
    },
    If {
        condition: ExprId,
        then_branch: StmtId,
        else_branch: Option<StmtId>,
        span: Span,
    },
    While {
        condition: ExprId,
        body: StmtId,
        span: Span,
    },
    For {
        initializer: Option<StmtId>,
        condition: Option<ExprId>,
        step: Option<ExprId>,
        body: StmtId,
        span: Span,
    },
    DoWhile {
        body: StmtId,
        condition: ExprId,
        span: Span,
    },
    Switch {
        value: ExprId,
        body: StmtId,
        span: Span,
    },
    Case {
        value: Option<ExprId>,
        span: Span,
    },
    With {
        object: ExprId,
        body: StmtId,
        span: Span,
    },
    Break(Span),
    Continue(Span),
    Empty(Span),
}

#[derive(Debug)]
pub struct Program {
    pub(crate) functions: Vec<Function>,
    pub(crate) strings: Vec<Arc<[u16]>>,
    pub(crate) octets: Vec<Box<[u8]>>,
    pub(crate) expressions: Vec<Expr>,
    pub(crate) statements: Vec<Statement>,
    pub(crate) roots: Vec<StmtId>,
    pub(crate) arguments: Vec<Argument>,
    pub(crate) dictionary_entries: Vec<(ExprId, ExprId)>,
    pub(crate) array_elements: Vec<ExprId>,
    pub(crate) span: Span,
}

impl Program {
    pub fn functions(&self) -> &[Function] {
        &self.functions
    }

    pub fn function(&self, id: FunctionId) -> &Function {
        &self.functions[id.0]
    }

    pub fn expression(&self, id: ExprId) -> &Expr {
        &self.expressions[id.0]
    }

    pub fn expressions(&self) -> &[Expr] {
        &self.expressions
    }

    pub fn statements(&self) -> &[Statement] {
        &self.statements
    }

    pub fn statement(&self, id: StmtId) -> &Statement {
        &self.statements[id.0]
    }

    pub fn roots(&self) -> &[StmtId] {
        &self.roots
    }

    pub fn arguments(&self, arguments: Arguments) -> Option<&[Argument]> {
        match arguments {
            Arguments::List { start, end } => Some(&self.arguments[start..end]),
            Arguments::ForwardOriginal => None,
        }
    }

    pub fn statement_span(&self, id: StmtId) -> Span {
        match *self.statement(id) {
            Statement::Var { span, .. }
            | Statement::Try { span, .. }
            | Statement::Throw { span, .. }
            | Statement::Function { span, .. }
            | Statement::Return { span, .. }
            | Statement::Block { span, .. }
            | Statement::If { span, .. }
            | Statement::While { span, .. }
            | Statement::For { span, .. }
            | Statement::DoWhile { span, .. }
            | Statement::Switch { span, .. }
            | Statement::Case { span, .. }
            | Statement::With { span, .. }
            | Statement::Break(span)
            | Statement::Continue(span)
            | Statement::Empty(span) => span,
            Statement::Expression(id) => self.expression(id).span,
        }
    }

    pub fn span(&self) -> Span {
        self.span
    }
}
