//! Definite initialization over reachable basic blocks, including loop back edges.
//! Storage is per basic block rather than per instruction.

use std::collections::VecDeque;

use crate::{
    CallSite, CatchHandler, Diagnostic, Instruction, Phase, Register, Span, module::find_handler,
};

const MAX_ANALYSIS_BYTES: usize = 8 * 1024 * 1024;

fn mark(bits: &mut [u64], register: Register) {
    bits[register.0 as usize / 64] |= 1 << (register.0 % 64);
}

fn ready(bits: &[u64], register: Register) -> bool {
    bits[register.0 as usize / 64] & (1 << (register.0 % 64)) != 0
}

pub(crate) fn definite_initialization(
    registers: u32,
    parameters: u32,
    code: &[Instruction],
    spans: &[Option<Span>],
    calls: &[CallSite],
    handlers: &[CatchHandler],
) -> Result<(), Diagnostic> {
    let error = |pc: usize, message| Diagnostic::new(Phase::Verify, spans[pc], message);
    let mut leaders = vec![false; code.len()];
    leaders[0] = true;
    for handler in handlers {
        leaders[handler.target as usize] = true;
    }
    for (pc, instruction) in code.iter().enumerate() {
        match instruction {
            Instruction::Jump { target } | Instruction::JumpIfFalse { target, .. } => {
                leaders[*target as usize] = true;
            }
            _ => {}
        }
        if matches!(
            instruction,
            Instruction::Jump { .. }
                | Instruction::JumpIfFalse { .. }
                | Instruction::Return { .. }
                | Instruction::Throw { .. }
                | Instruction::Call { .. }
                | Instruction::Eval { .. }
        ) && pc + 1 < code.len()
        {
            leaders[pc + 1] = true;
        }
    }
    let starts: Vec<_> = leaders
        .iter()
        .enumerate()
        .filter_map(|(pc, &leader)| leader.then_some(pc))
        .collect();
    let words = (registers as usize).div_ceil(64);
    if starts.len() * words * size_of::<u64>() > MAX_ANALYSIS_BYTES {
        return Err(error(
            0,
            "control-flow initialization analysis exceeds the memory limit",
        ));
    }
    let mut inputs = vec![0_u64; starts.len() * words];
    for parameter in 1..=parameters {
        mark(&mut inputs[..words], Register(parameter));
    }
    let mut seen = vec![false; starts.len()];
    let mut queued = vec![false; starts.len()];
    let mut pending = VecDeque::new();
    let mut output = vec![0_u64; words];
    let mut exceptional = vec![0_u64; words];
    seen[0] = true; // Only parameter slots are initialized on the entry path.
    queued[0] = true;
    pending.push_back(0);

    while let Some(block) = pending.pop_front() {
        queued[block] = false;
        let start = starts[block];
        let end = starts.get(block + 1).copied().unwrap_or(code.len());
        output.copy_from_slice(&inputs[block * words..(block + 1) * words]);
        for (pc, instruction) in code.iter().enumerate().take(end).skip(start) {
            if matches!(
                instruction,
                Instruction::Call { .. } | Instruction::Eval { .. } | Instruction::Throw { .. }
            ) {
                if let Some(handler) = find_handler(handlers, pc) {
                    exceptional.copy_from_slice(&output);
                    mark(&mut exceptional, handler.exception);
                    let next = starts
                        .binary_search(&(handler.target as usize))
                        .expect("catch block leader");
                    merge(
                        next,
                        &exceptional,
                        &mut inputs,
                        &mut seen,
                        &mut queued,
                        &mut pending,
                    );
                }
            }
            if let Some(register) = instruction.writes(calls) {
                mark(&mut output, register);
            }
        }
        let successors = match code[end - 1] {
            Instruction::Return { .. } | Instruction::Throw { .. } => [None, None],
            Instruction::Jump { target } => [Some(target as usize), None],
            Instruction::JumpIfFalse { target, .. } => [Some(target as usize), Some(end)],
            _ => [Some(end), None],
        };
        for target in successors.into_iter().flatten() {
            // Structural validation guarantees all targets and fallthroughs exist.
            let next = starts
                .binary_search(&target)
                .expect("target is a block leader");
            merge(
                next,
                &output,
                &mut inputs,
                &mut seen,
                &mut queued,
                &mut pending,
            );
        }
    }

    // Check reads only after the intersection of every reachable predecessor has
    // stabilized. Checking on the first visit would accept some invalid joins.
    for (block, &start) in starts.iter().enumerate().filter(|(block, _)| seen[*block]) {
        let end = starts.get(block + 1).copied().unwrap_or(code.len());
        output.copy_from_slice(&inputs[block * words..(block + 1) * words]);
        for (pc, instruction) in code.iter().enumerate().take(end).skip(start) {
            for register in instruction.reads().into_iter().flatten() {
                if !ready(&output, register) {
                    return Err(error(
                        pc,
                        "read of a register not initialized on every incoming path",
                    ));
                }
            }
            if let Instruction::Call { site } = *instruction {
                if calls[site as usize]
                    .reads()
                    .any(|register| !ready(&output, register))
                {
                    return Err(error(
                        pc,
                        "call operand is not initialized on every incoming path",
                    ));
                }
            }
            if let Some(register) = instruction.writes(calls) {
                mark(&mut output, register);
            }
        }
    }
    Ok(())
}

fn merge(
    next: usize,
    output: &[u64],
    inputs: &mut [u64],
    seen: &mut [bool],
    queued: &mut [bool],
    pending: &mut VecDeque<usize>,
) {
    if next == 0 {
        return; // A back edge cannot change the initial entry path.
    }
    let words = output.len();
    let input = &mut inputs[next * words..(next + 1) * words];
    let mut changed = !seen[next];
    if !seen[next] {
        input.copy_from_slice(output);
        seen[next] = true;
    } else {
        for (before, &out) in input.iter_mut().zip(output) {
            let after = *before & out;
            changed |= after != *before;
            *before = after;
        }
    }
    if changed && !queued[next] {
        pending.push_back(next);
        queued[next] = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Module;

    fn verify(code: Vec<Instruction>) -> Result<Module, Diagnostic> {
        let spans = vec![None; code.len()];
        Module::new(2, code, spans)
    }

    #[test]
    fn both_sides_of_a_join_must_initialize_the_result() {
        let mut code = vec![
            Instruction::LoadInt {
                dst: Register(0),
                value: 1,
            },
            Instruction::JumpIfFalse {
                condition: Register(0),
                target: 4,
            },
            Instruction::LoadInt {
                dst: Register(1),
                value: 10,
            },
            Instruction::Jump { target: 5 },
            Instruction::LoadInt {
                dst: Register(1),
                value: 20,
            },
            Instruction::Return { src: Register(1) },
        ];
        assert!(verify(code.clone()).is_ok());
        code[4] = Instruction::Jump { target: 5 };
        assert!(
            verify(code)
                .unwrap_err()
                .message
                .contains("every incoming path")
        );
    }

    #[test]
    fn loop_body_cannot_initialize_the_first_iteration() {
        assert!(
            verify(vec![
                Instruction::Move {
                    dst: Register(0),
                    src: Register(1)
                },
                Instruction::LoadInt {
                    dst: Register(1),
                    value: 1
                },
                Instruction::Jump { target: 0 },
                Instruction::Return { src: Register(0) },
            ])
            .is_err()
        );
        assert!(
            verify(vec![
                Instruction::LoadInt {
                    dst: Register(0),
                    value: 1
                },
                Instruction::JumpIfFalse {
                    condition: Register(0),
                    target: 4
                },
                Instruction::LoadInt {
                    dst: Register(1),
                    value: 1
                },
                Instruction::Jump { target: 1 },
                Instruction::Return { src: Register(1) },
            ])
            .is_err()
        );
    }

    #[test]
    fn unreachable_reads_are_safe_but_invalid_register_indices_are_not() {
        assert!(
            verify(vec![
                Instruction::Jump { target: 0 },
                Instruction::Return { src: Register(1) },
            ])
            .is_ok()
        );
        assert!(
            verify(vec![
                Instruction::Jump { target: 0 },
                Instruction::Return { src: Register(2) },
            ])
            .is_err()
        );
    }

    #[test]
    fn oversized_analysis_is_rejected_before_allocating_the_matrix() {
        let mut code = vec![Instruction::Jump { target: 0 }; 2_000];
        code.push(Instruction::Return { src: Register(0) });
        let spans = vec![None; code.len()];
        assert!(
            Module::new(crate::ir::MAX_REGISTERS, code, spans)
                .unwrap_err()
                .message
                .contains("memory limit")
        );
    }
}
