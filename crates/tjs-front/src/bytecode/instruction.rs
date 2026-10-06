//! Opcode identities from krkrz tjsInterCodeGen.h; unknown words are errors.
use super::read::{Context, Object, Value, error};
use tjs_core::Diagnostic;

#[derive(Clone, Copy, Debug)]
pub(super) struct Decoded {
    pub pc: u32,
    pub length: u32,
    pub op: Opcode,
}

#[derive(Clone, Copy, Debug)]
pub(super) enum Form {
    Register,
    Direct,
    Indirect,
    Property,
}

#[derive(Clone, Copy, Debug)]
pub(super) enum Operation {
    Inc,
    Dec,
    Lor,
    Land,
    Bor,
    Bxor,
    Band,
    Sar,
    Sal,
    Sr,
    Add,
    Sub,
    Mod,
    Div,
    Idiv,
    Mul,
}

impl Opcode {
    pub fn operation(self) -> Option<(Operation, Form)> {
        let value = self as i16 - Self::Inc as i16;
        const OPERATIONS: &[Operation] = &[
            Operation::Inc,
            Operation::Dec,
            Operation::Lor,
            Operation::Land,
            Operation::Bor,
            Operation::Bxor,
            Operation::Band,
            Operation::Sar,
            Operation::Sal,
            Operation::Sr,
            Operation::Add,
            Operation::Sub,
            Operation::Mod,
            Operation::Div,
            Operation::Idiv,
            Operation::Mul,
        ];
        if !(0..64).contains(&value) {
            return None;
        }
        let form = match value % 4 {
            0 => Form::Register,
            1 => Form::Direct,
            2 => Form::Indirect,
            _ => Form::Property,
        };
        Some((OPERATIONS[value as usize / 4], form))
    }
}

pub(super) fn decode(object: &Object) -> Result<Vec<Decoded>, Diagnostic> {
    let code = &object.code;
    let fail = |pc, message| error(object.code_offset + pc * 2, message);
    if code.is_empty() && object.context != Context::Property {
        return Err(fail(0, "executable context has no instructions"));
    }
    let mut instructions = Vec::new();
    let mut targets = Vec::new();
    let mut pc = 0;
    while pc < code.len() {
        let op = Opcode::read(code[pc]).ok_or_else(|| fail(pc, "unknown opcode"))?;
        let mut cursor = pc + 1;
        let mut next = || {
            let value = code
                .get(cursor)
                .copied()
                .ok_or_else(|| fail(pc, "truncated instruction"))?;
            cursor += 1;
            Ok::<_, Diagnostic>(value)
        };
        let register = |value: i16| {
            let value = i32::from(value);
            if value < -(object.variables as i32 + object.reserve as i32)
                || value > object.frames as i32
            {
                Err(fail(pc, "register outside declared frame"))
            } else {
                Ok(())
            }
        };
        let constant = |value: i16, string: bool| {
            let item = usize::try_from(value)
                .ok()
                .and_then(|i| object.values.get(i))
                .ok_or_else(|| fail(pc, "constant index outside object data"))?;
            if string && !matches!(item, Value::String(_)) {
                return Err(fail(pc, "direct member name is not a string constant"));
            }
            Ok(())
        };
        let target = |offset: i16| {
            let target = pc as i64 + i64::from(offset);
            if target < 0 || target >= code.len() as i64 {
                return Err(fail(pc, "branch outside code"));
            }
            Ok(target as u32)
        };
        if let Some((operation, form)) = op.operation() {
            let unary = matches!(operation, Operation::Inc | Operation::Dec);
            register(next()?)?;
            match form {
                Form::Register => {
                    if !unary {
                        register(next()?)?;
                    }
                }
                Form::Direct | Form::Indirect => {
                    register(next()?)?;
                    let name = next()?;
                    if matches!(form, Form::Direct) {
                        constant(name, true)?;
                    } else {
                        register(name)?;
                    }
                    if !unary {
                        register(next()?)?;
                    }
                }
                Form::Property => {
                    register(next()?)?;
                    if !unary {
                        register(next()?)?;
                    }
                }
            }
        } else {
            use Opcode::*;
            match op {
                Nop | Nf | Ret | Extry | Regmember | Debugger => {}
                Const => {
                    register(next()?)?;
                    constant(next()?, false)?;
                }
                Cp | Ceq | Cdeq | Clt | Cgt | Chkins | Setp | Getp | Chgthis | Addci => {
                    register(next()?)?;
                    register(next()?)?;
                }
                Cl | Tt | Tf | Setf | Setnf | Lnot | Bnot | Typeof | Eval | Eexp | Asc | Chr
                | Num | Chs | Inv | Chkinv | Int | Real | Str | Octet | Srv | Throw | Global => {
                    register(next()?)?;
                }
                Ccl => {
                    let start = next()?;
                    register(start)?;
                    let count = next()?;
                    if count < 0 {
                        return Err(fail(pc, "negative clear count"));
                    }
                    if count > 0 {
                        let end = i32::from(start) + i32::from(count) - 1;
                        register(
                            i16::try_from(end)
                                .map_err(|_| fail(pc, "clear range exceeds register encoding"))?,
                        )?;
                    }
                }
                Jf | Jnf | Jmp => targets.push(target(next()?)?),
                Entry => {
                    targets.push(target(next()?)?);
                    register(next()?)?;
                }
                Call | Calld | Calli | New => {
                    register(next()?)?;
                    register(next()?)?;
                    if matches!(op, Calld | Calli) {
                        let name = next()?;
                        if op == Calld {
                            constant(name, true)?;
                        } else {
                            register(name)?;
                        }
                    }
                    match next()? {
                        -1 => {}
                        -2 => {
                            let count = next()?;
                            if count < 0 {
                                return Err(fail(pc, "negative expansion descriptor count"));
                            }
                            for _ in 0..count {
                                let kind = next()?;
                                let value = next()?;
                                match kind {
                                    0 | 1 => register(value)?,
                                    2 if object.unnamed.is_some() => {}
                                    _ => {
                                        return Err(fail(
                                            pc,
                                            "invalid argument expansion descriptor",
                                        ));
                                    }
                                }
                            }
                        }
                        count if count >= 0 => {
                            for _ in 0..count {
                                register(next()?)?;
                            }
                        }
                        _ => return Err(fail(pc, "invalid call argument count")),
                    }
                }
                Gpd | Gpds | Typeofd | Deld => {
                    register(next()?)?;
                    register(next()?)?;
                    constant(next()?, true)?;
                }
                Spd | Spde | Spdeh | Spds => {
                    register(next()?)?;
                    constant(next()?, true)?;
                    register(next()?)?;
                }
                Gpi | Gpis | Typeofi | Deli | Spi | Spie | Spis => {
                    register(next()?)?;
                    register(next()?)?;
                    register(next()?)?;
                }
                _ => unreachable!("operation family handled above"),
            }
        }
        // The compiler reserves -1/-2 for this and its scope proxy. Name
        // lowering relies on that invariant; reject hand-written mutations
        // instead of silently executing them with a different frame context.
        let writes_first = op.operation().is_some()
            || matches!(
                op,
                Opcode::Const
                    | Opcode::Cp
                    | Opcode::Cl
                    | Opcode::Setf
                    | Opcode::Setnf
                    | Opcode::Lnot
                    | Opcode::Bnot
                    | Opcode::Typeof
                    | Opcode::Typeofd
                    | Opcode::Typeofi
                    | Opcode::Eval
                    | Opcode::Chkins
                    | Opcode::Asc
                    | Opcode::Chr
                    | Opcode::Num
                    | Opcode::Chs
                    | Opcode::Inv
                    | Opcode::Chkinv
                    | Opcode::Int
                    | Opcode::Real
                    | Opcode::Str
                    | Opcode::Octet
                    | Opcode::Call
                    | Opcode::Calld
                    | Opcode::Calli
                    | Opcode::New
                    | Opcode::Gpd
                    | Opcode::Gpi
                    | Opcode::Gpds
                    | Opcode::Gpis
                    | Opcode::Getp
                    | Opcode::Deld
                    | Opcode::Deli
                    | Opcode::Chgthis
                    | Opcode::Global
            );
        if (writes_first && matches!(code[pc + 1], -2 | -1))
            || (op == Opcode::Entry && matches!(code[pc + 2], -2 | -1))
            || (op == Opcode::Ccl
                && (-2..=-1).any(|reserved| {
                    i32::from(code[pc + 1]) <= reserved
                        && reserved < i32::from(code[pc + 1]) + i32::from(code[pc + 2])
                }))
        {
            return Err(fail(pc, "write to a reserved frame context slot"));
        }
        instructions.push(Decoded {
            pc: pc as u32,
            length: (cursor - pc) as u32,
            op,
        });
        pc = cursor;
    }
    let boundary = |pc| instructions.binary_search_by_key(&pc, |i| i.pc).is_ok();
    for target in targets
        .into_iter()
        .chain(object.super_entries.iter().copied())
    {
        if !boundary(target) {
            return Err(fail(target as usize, "target points inside an instruction"));
        }
    }
    let mut previous = None;
    for &(pc, _) in &object.source_positions {
        if (pc != code.len() as u32 && !boundary(pc)) || previous.is_some_and(|last| pc < last) {
            return Err(fail(
                pc as usize,
                "invalid source position code offset/order",
            ));
        }
        previous = Some(pc);
    }
    Ok(instructions)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i16)]
pub(super) enum Opcode {
    Nop = 0,
    Const = 1,
    Cp = 2,
    Cl = 3,
    Ccl = 4,
    Tt = 5,
    Tf = 6,
    Ceq = 7,
    Cdeq = 8,
    Clt = 9,
    Cgt = 10,
    Setf = 11,
    Setnf = 12,
    Lnot = 13,
    Nf = 14,
    Jf = 15,
    Jnf = 16,
    Jmp = 17,
    Inc = 18,
    Incpd = 19,
    Incpi = 20,
    Incp = 21,
    Dec = 22,
    Decpd = 23,
    Decpi = 24,
    Decp = 25,
    Lor = 26,
    Lorpd = 27,
    Lorpi = 28,
    Lorp = 29,
    Land = 30,
    Landpd = 31,
    Landpi = 32,
    Landp = 33,
    Bor = 34,
    Borpd = 35,
    Borpi = 36,
    Borp = 37,
    Bxor = 38,
    Bxorpd = 39,
    Bxorpi = 40,
    Bxorp = 41,
    Band = 42,
    Bandpd = 43,
    Bandpi = 44,
    Bandp = 45,
    Sar = 46,
    Sarpd = 47,
    Sarpi = 48,
    Sarp = 49,
    Sal = 50,
    Salpd = 51,
    Salpi = 52,
    Salp = 53,
    Sr = 54,
    Srpd = 55,
    Srpi = 56,
    Srp = 57,
    Add = 58,
    Addpd = 59,
    Addpi = 60,
    Addp = 61,
    Sub = 62,
    Subpd = 63,
    Subpi = 64,
    Subp = 65,
    Mod = 66,
    Modpd = 67,
    Modpi = 68,
    Modp = 69,
    Div = 70,
    Divpd = 71,
    Divpi = 72,
    Divp = 73,
    Idiv = 74,
    Idivpd = 75,
    Idivpi = 76,
    Idivp = 77,
    Mul = 78,
    Mulpd = 79,
    Mulpi = 80,
    Mulp = 81,
    Bnot = 82,
    Typeof = 83,
    Typeofd = 84,
    Typeofi = 85,
    Eval = 86,
    Eexp = 87,
    Chkins = 88,
    Asc = 89,
    Chr = 90,
    Num = 91,
    Chs = 92,
    Inv = 93,
    Chkinv = 94,
    Int = 95,
    Real = 96,
    Str = 97,
    Octet = 98,
    Call = 99,
    Calld = 100,
    Calli = 101,
    New = 102,
    Gpd = 103,
    Spd = 104,
    Spde = 105,
    Spdeh = 106,
    Gpi = 107,
    Spi = 108,
    Spie = 109,
    Gpds = 110,
    Spds = 111,
    Gpis = 112,
    Spis = 113,
    Setp = 114,
    Getp = 115,
    Deld = 116,
    Deli = 117,
    Srv = 118,
    Ret = 119,
    Entry = 120,
    Extry = 121,
    Throw = 122,
    Chgthis = 123,
    Global = 124,
    Addci = 125,
    Regmember = 126,
    Debugger = 127,
}

impl Opcode {
    fn read(word: i16) -> Option<Self> {
        const ALL: &[Opcode] = &[
            Opcode::Nop,
            Opcode::Const,
            Opcode::Cp,
            Opcode::Cl,
            Opcode::Ccl,
            Opcode::Tt,
            Opcode::Tf,
            Opcode::Ceq,
            Opcode::Cdeq,
            Opcode::Clt,
            Opcode::Cgt,
            Opcode::Setf,
            Opcode::Setnf,
            Opcode::Lnot,
            Opcode::Nf,
            Opcode::Jf,
            Opcode::Jnf,
            Opcode::Jmp,
            Opcode::Inc,
            Opcode::Incpd,
            Opcode::Incpi,
            Opcode::Incp,
            Opcode::Dec,
            Opcode::Decpd,
            Opcode::Decpi,
            Opcode::Decp,
            Opcode::Lor,
            Opcode::Lorpd,
            Opcode::Lorpi,
            Opcode::Lorp,
            Opcode::Land,
            Opcode::Landpd,
            Opcode::Landpi,
            Opcode::Landp,
            Opcode::Bor,
            Opcode::Borpd,
            Opcode::Borpi,
            Opcode::Borp,
            Opcode::Bxor,
            Opcode::Bxorpd,
            Opcode::Bxorpi,
            Opcode::Bxorp,
            Opcode::Band,
            Opcode::Bandpd,
            Opcode::Bandpi,
            Opcode::Bandp,
            Opcode::Sar,
            Opcode::Sarpd,
            Opcode::Sarpi,
            Opcode::Sarp,
            Opcode::Sal,
            Opcode::Salpd,
            Opcode::Salpi,
            Opcode::Salp,
            Opcode::Sr,
            Opcode::Srpd,
            Opcode::Srpi,
            Opcode::Srp,
            Opcode::Add,
            Opcode::Addpd,
            Opcode::Addpi,
            Opcode::Addp,
            Opcode::Sub,
            Opcode::Subpd,
            Opcode::Subpi,
            Opcode::Subp,
            Opcode::Mod,
            Opcode::Modpd,
            Opcode::Modpi,
            Opcode::Modp,
            Opcode::Div,
            Opcode::Divpd,
            Opcode::Divpi,
            Opcode::Divp,
            Opcode::Idiv,
            Opcode::Idivpd,
            Opcode::Idivpi,
            Opcode::Idivp,
            Opcode::Mul,
            Opcode::Mulpd,
            Opcode::Mulpi,
            Opcode::Mulp,
            Opcode::Bnot,
            Opcode::Typeof,
            Opcode::Typeofd,
            Opcode::Typeofi,
            Opcode::Eval,
            Opcode::Eexp,
            Opcode::Chkins,
            Opcode::Asc,
            Opcode::Chr,
            Opcode::Num,
            Opcode::Chs,
            Opcode::Inv,
            Opcode::Chkinv,
            Opcode::Int,
            Opcode::Real,
            Opcode::Str,
            Opcode::Octet,
            Opcode::Call,
            Opcode::Calld,
            Opcode::Calli,
            Opcode::New,
            Opcode::Gpd,
            Opcode::Spd,
            Opcode::Spde,
            Opcode::Spdeh,
            Opcode::Gpi,
            Opcode::Spi,
            Opcode::Spie,
            Opcode::Gpds,
            Opcode::Spds,
            Opcode::Gpis,
            Opcode::Spis,
            Opcode::Setp,
            Opcode::Getp,
            Opcode::Deld,
            Opcode::Deli,
            Opcode::Srv,
            Opcode::Ret,
            Opcode::Entry,
            Opcode::Extry,
            Opcode::Throw,
            Opcode::Chgthis,
            Opcode::Global,
            Opcode::Addci,
            Opcode::Regmember,
            Opcode::Debugger,
        ];
        usize::try_from(word).ok().and_then(|i| ALL.get(i)).copied()
    }
}
