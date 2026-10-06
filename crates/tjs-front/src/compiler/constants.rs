use super::Constants;
use crate::ast::{ExprId, ExprKind, Program, UnaryOp};
use tjs_core::{Constant, ConstantValue, Diagnostic, Heap, ObjRef, Phase, Value, value};

impl Constants {
    pub(super) fn containers(&mut self, program: &Program) -> Result<(), Diagnostic> {
        // The parser's flat arena places children before parents. Convert once
        // per source literal, including literals emitted by several base resolvers.
        let mut scratch = None;
        for (id, expression) in program.expressions().iter().enumerate() {
            let constant = match expression.kind {
                ExprKind::ConstantArray { start, end } => {
                    let scratch = scratch.get_or_insert_with(Heap::new);
                    let values = program.array_elements[start..end]
                        .iter()
                        .map(|&id| self.element(program, id, scratch))
                        .collect::<Result<_, _>>()?;
                    Constant::Array(values)
                }
                ExprKind::ConstantDictionary { start, end } => {
                    let scratch = scratch.get_or_insert_with(Heap::new);
                    let mut entries = Vec::with_capacity(end - start);
                    for &(key, element) in &program.dictionary_entries[start..end] {
                        let name = if let ExprKind::String(index) = program.expression(key).kind {
                            program.strings[index as usize].clone()
                        } else {
                            let scalar = scalar(program, key, scratch)?;
                            let text = value::to_string(scratch, scalar).map_err(|error| {
                                Diagnostic::new(
                                    Phase::Compile,
                                    program.expression(key).span,
                                    error.to_string(),
                                )
                            })?;
                            let Value::Str(text) = text else {
                                unreachable!("string conversion")
                            };
                            scratch.string(text).expect("temporary string").into()
                        };
                        entries.push((name, self.element(program, element, scratch)?));
                    }
                    Constant::Dictionary(entries.into())
                }
                _ => continue,
            };
            self.containers.insert(id, self.values.len() as u32);
            self.values.push(constant);
        }
        Ok(())
    }

    fn element(
        &self,
        program: &Program,
        id: ExprId,
        scratch: &mut Heap,
    ) -> Result<ConstantValue, Diagnostic> {
        Ok(match program.expression(id).kind {
            ExprKind::ConstantArray { .. } | ExprKind::ConstantDictionary { .. } => {
                ConstantValue::Reference(self.containers[&id.0])
            }
            ExprKind::String(index) => ConstantValue::Reference(index),
            ExprKind::Octet(index) => {
                ConstantValue::Reference(program.strings.len() as u32 + index)
            }
            _ => match scalar(program, id, scratch)? {
                Value::Void => ConstantValue::Void,
                Value::Int(value) => ConstantValue::Int(value),
                Value::Real(value) => ConstantValue::Real(value),
                Value::Obj(_) => ConstantValue::Null,
                _ => unreachable!("scalar constant"),
            },
        })
    }
}

// Reuse VM conversions for signed literals and dictionary keys; the temporary
// heap never escapes into portable module data.
fn scalar(program: &Program, id: ExprId, heap: &mut Heap) -> Result<Value, Diagnostic> {
    let expression = program.expression(id);
    Ok(match expression.kind {
        ExprKind::Void => Value::Void,
        ExprKind::Integer(value) => Value::Int(value),
        ExprKind::Real(bits) => Value::Real(f64::from_bits(bits)),
        ExprKind::Null => Value::Obj(ObjRef::default()),
        ExprKind::String(index) => {
            Value::Str(heap.alloc_string(program.strings[index as usize].as_ref()))
        }
        ExprKind::Octet(index) => {
            Value::Octet(heap.alloc_octet(program.octets[index as usize].clone()))
        }
        ExprKind::Negate(inner)
        | ExprKind::Unary {
            op: UnaryOp::Number,
            inner,
        } => {
            let input = scalar(program, inner, heap)?;
            let result = if matches!(expression.kind, ExprKind::Negate(_)) {
                value::negate_in(heap, input)
            } else {
                value::to_number(heap, input)
            };
            result.map_err(|error| {
                Diagnostic::new(Phase::Compile, expression.span, error.to_string())
            })?
        }
        _ => unreachable!("constant grammar"),
    })
}
