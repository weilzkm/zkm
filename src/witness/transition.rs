use anyhow::bail;
use log::log_enabled;
use plonky2::field::types::Field;

use super::memory::{MemoryOp, MemoryOpKind};
use super::util::fill_channel_with_value;
use crate::cpu::columns::CpuColumnsView;
use crate::cpu::kernel::KERNEL;
use crate::generation::state::GenerationState;
use crate::memory::segments::Segment;
use crate::witness::errors::ProgramError;
use crate::witness::memory::MemoryAddress;
use crate::witness::memory::MemoryChannel::GeneralPurpose;
use crate::witness::operation::*;
use crate::witness::state::RegistersState;
use crate::witness::util::mem_read_code_with_log_and_fill;
use crate::{arithmetic, logic};

fn read_code_memory<F: Field>(
    state: &mut GenerationState<F>,
    row: &mut CpuColumnsView<F>,
) -> u32 {
    let code_context = state.registers.code_context();
    row.code_context = F::from_canonical_usize(code_context);

    let address = MemoryAddress::new(code_context, Segment::Code, state.registers.program_counter);
    let (opcode, mem_log) = mem_read_code_with_log_and_fill(address, state, row);
    log::debug!("read_code_memory: PC {} op: {:?}, {:?}", state.registers.program_counter, opcode, mem_log);

    state.traces.push_memory(mem_log);

    opcode
}

fn decode(registers: RegistersState, insn: u32) -> Result<Operation, ProgramError> {
    // FIXME: use big endian
    let insn = insn.to_be();
    let opcode = ((insn >> 26) & 0x3F).to_le_bytes()[0];
    let func = (insn & 0x3F).to_le_bytes()[0];
    let rt = ((insn >> 16) & 0x1F).to_le_bytes()[0];
    let rs = ((insn >> 21) & 0x1F).to_le_bytes()[0];
    let rd = ((insn >> 11) & 0x1F).to_le_bytes()[0];
    let sa = ((insn >> 6) & 0x1F).to_le_bytes()[0];
    let offset = insn & 0xffff;
    let target = insn & 0x3ffffff;
    println!("decode: insn {:X}, opcode {:X}, func {:X}", insn, opcode, func);

    match (opcode, func, registers.is_kernel) {
        (0b000000, 0b100000, _) => Ok(Operation::BinaryArithmetic(arithmetic::BinaryOperator::ADD, rs, rt, rd)), // ADD: rd = rs+rt
        (0b000000, 0b000000, _) => Ok(Operation::BinaryArithmetic(arithmetic::BinaryOperator::SLL, rt, sa, rd)), // SLL: rd = rt << sa
        (0b000000, 0b100000, _) => Ok(Operation::Jump(0u8, rs)), // JR
        (0x00, 0x08, _) => Ok(Operation::Jump(0u8, rs)), // JR
        (0x00, 0x09, _) => Ok(Operation::Jump(rd, rs)),  // JALR
        (0x01, _, _) => {
            if rt == 1 {
                Ok(Operation::Branch(Cond::GE, rs, 0u8, offset)) // BGEZ
            } else if rt == 0 {
                Ok(Operation::Branch(Cond::LT, rs, 0u8, offset)) // BLTZ
            } else {
                Err(ProgramError::InvalidOpcode)
            }
        }
        (0x02, _, _) => Ok(Operation::Jumpi(0u8, target)), // J
        (0x03, _, _) => Ok(Operation::Jumpi(31u8, target)), // JAL
        (0x04, _, _) => Ok(Operation::Branch(Cond::EQ, rs, rt, offset)), // BEQ
        (0x05, _, _) => Ok(Operation::Branch(Cond::NE, rs, rt, offset)), // BNE
        (0x06, _, _) => Ok(Operation::Branch(Cond::LE, rs, 0u8, offset)), // BLEZ
        (0x07, _, _) => Ok(Operation::Branch(Cond::GT, rs, 0u8, offset)), // BGTZ
        (0b100011, _, _) => Ok(Operation::Mload32Bytes(rs, rt, offset)), // LW
        _ => {
            log::warn!("Decode: invalid opcode: {} {}", opcode, func);
            Err(ProgramError::InvalidOpcode)
        }
    }
}

fn fill_op_flag<F: Field>(op: Operation, row: &mut CpuColumnsView<F>) {
    let flags = &mut row.op;
    *match op {
        Operation::Swap(_) => &mut flags.swap,
        Operation::Iszero | Operation::Eq => &mut flags.eq_iszero,
        Operation::Not => &mut flags.not,
        Operation::Syscall(_, _, _) => &mut flags.syscall,
        Operation::BinaryLogic(_) => &mut flags.logic_op,
        Operation::BinaryArithmetic(..) => &mut flags.binary_op,
        Operation::KeccakGeneral => &mut flags.keccak_general,
        Operation::ProverInput => &mut flags.prover_input,
        Operation::Jump(_, _) | Operation::Jumpi(_, _) => &mut flags.jumps,
        Operation::Branch(_, _, _, _) => &mut flags.branch,
        Operation::Pc => &mut flags.pc,
        Operation::GetContext => &mut flags.get_context,
        Operation::SetContext => &mut flags.set_context,
        Operation::Mload32Bytes(_, _, _) => &mut flags.mload_32bytes,
        Operation::Mstore32Bytes => &mut flags.mstore_32bytes,
        Operation::ExitKernel => &mut flags.exit_kernel,
        Operation::MloadGeneral | Operation::MstoreGeneral => &mut flags.m_op_general,
    } = F::ONE;
}

fn perform_op<F: Field>(
    state: &mut GenerationState<F>,
    op: Operation,
    row: CpuColumnsView<F>,
) -> Result<(), ProgramError> {
    log::debug!("perform_op {:?}", op);
    match op {
        Operation::Swap(n) => generate_swap(n, state, row)?,
        Operation::Iszero => generate_iszero(state, row)?,
        Operation::Not => generate_not(state, row)?,
        Operation::Syscall(opcode, stack_values_read, stack_len_increased) => {
            generate_syscall(opcode, stack_values_read, stack_len_increased, state, row)?
        }
        Operation::Eq => generate_eq(state, row)?,
        Operation::BinaryLogic(binary_logic_op) => {
            generate_binary_logic_op(binary_logic_op, state, row)?
        }
        Operation::BinaryArithmetic(op, rs, rt, rd) => generate_binary_arithmetic_op(rs, rt, rd, op, state, row)?,
        Operation::KeccakGeneral => generate_keccak_general(state, row)?,
        Operation::ProverInput => generate_prover_input(state, row)?,
        Operation::Jump(link, target) => generate_jump(link, target, state, row)?,
        Operation::Jumpi(link, target) => generate_jumpi(link, target, state, row)?,
        Operation::Branch(cond, input1, input2, target) => {
            generate_branch(cond, input1, input2, target, state, row)?
        },
        Operation::Pc => generate_pc(state, row)?,
        Operation::GetContext => generate_get_context(state, row)?,
        Operation::SetContext => generate_set_context(state, row)?,
        Operation::Mload32Bytes(base, rt, offset) => generate_mload_32bytes(base, rt, offset, state, row)?,
        Operation::Mstore32Bytes => generate_mstore_32bytes(state, row)?,
        Operation::ExitKernel => generate_exit_kernel(state, row)?,
        Operation::MloadGeneral => generate_mload_general(state, row)?,
        Operation::MstoreGeneral => generate_mstore_general(state, row)?,
    };


    state.registers.program_counter += match op {
        Operation::Syscall(_, _, _) | Operation::ExitKernel => 0,
        Operation::Jump(_, _) => 0,
        Operation::Jumpi(_, _) => 0,
        Operation::Branch(_, _, _, _) => 0,
        _ => 4,
    };

    /*
    state.registers.gas_used += gas_to_charge(op);
    */

    Ok(())
}

/// Row that has the correct values for system registers and the code channel, but is otherwise
/// blank. It fulfills the constraints that are common to successful operations and the exception
/// operation. It also returns the opcode.
fn base_row<F: Field>(state: &mut GenerationState<F>) -> (CpuColumnsView<F>, u32) {
    let mut row: CpuColumnsView<F> = CpuColumnsView::default();
    row.clock = F::from_canonical_usize(state.traces.clock());
    row.context = F::from_canonical_usize(state.registers.context);
    row.program_counter = F::from_canonical_usize(state.registers.program_counter);
    row.is_kernel_mode = F::from_bool(state.registers.is_kernel);
    /*
    row.gas = [
        F::from_canonical_u32(state.registers.gas_used as u32),
        F::from_canonical_u32((state.registers.gas_used >> 32) as u32),
    ];
    row.stack_len = F::from_canonical_usize(state.registers.stack_len);
    fill_channel_with_value(&mut row, 0, state.registers.stack_top);
    */

    let opcode = read_code_memory(state, &mut row);
    (row, opcode)
}

fn try_perform_instruction<F: Field>(state: &mut GenerationState<F>) -> Result<(), ProgramError> {
    let (mut row, opcode) = base_row(state);
    let op = decode(state.registers, opcode)?;

    if state.registers.is_kernel {
        log_kernel_instruction(state, op);
    } else {
        log::debug!("User instruction: {:?}", op);
    }

    fill_op_flag(op, &mut row);

    // FIXME: decode instruction data, and load IMM and input data into registers

    /*
    if state.registers.is_stack_top_read {
        let channel = &mut row.mem_channels[0];
        channel.used = F::ONE;
        channel.is_read = F::ONE;
        channel.addr_context = F::from_canonical_usize(state.registers.context);
        channel.addr_segment = F::from_canonical_usize(Segment::Stack as usize);
        channel.addr_virtual = F::from_canonical_usize(state.registers.stack_len - 1);

        let address = MemoryAddress {
            context: state.registers.context,
            segment: Segment::Stack as usize,
            virt: state.registers.stack_len - 1,
        };

        let mem_op = MemoryOp::new(
            GeneralPurpose(0),
            state.traces.clock(),
            address,
            MemoryOpKind::Read,
            state.registers.stack_top,
        );
        state.traces.push_memory(mem_op);
        state.registers.is_stack_top_read = false;
    }

    if state.registers.is_kernel {
        row.stack_len_bounds_aux = F::ZERO;
    } else {
        let disallowed_len = F::from_canonical_usize(MAX_USER_STACK_SIZE + 1);
        let diff = row.stack_len - disallowed_len;
        if let Some(inv) = diff.try_inverse() {
            row.stack_len_bounds_aux = inv;
        } else {
            // This is a stack overflow that should have been caught earlier.
            return Err(ProgramError::InterpreterError);
        }
    }

    // Might write in general CPU columns when it shouldn't, but the correct values will
    // overwrite these ones during the op generation.
    if let Some(special_len) = get_op_special_length(op) {
        let special_len = F::from_canonical_usize(special_len);
        let diff = row.stack_len - special_len;
        if let Some(inv) = diff.try_inverse() {
            row.general.stack_mut().stack_inv = inv;
            row.general.stack_mut().stack_inv_aux = F::ONE;
            state.registers.is_stack_top_read = true;
        }
    } else if let Some(inv) = row.stack_len.try_inverse() {
        row.general.stack_mut().stack_inv = inv;
        row.general.stack_mut().stack_inv_aux = F::ONE;
    }

    */
    perform_op(state, op, row)
}

fn log_kernel_instruction<F: Field>(state: &GenerationState<F>, op: Operation) {
    // The logic below is a bit costly, so skip it if debug logs aren't enabled.
    if !log_enabled!(log::Level::Debug) {
        return;
    }

    let pc = state.registers.program_counter;
    let is_interesting_offset = KERNEL
        .offset_label(pc)
        .filter(|label| !label.starts_with("halt"))
        .is_some();
    let level = if is_interesting_offset {
        log::Level::Debug
    } else {
        log::Level::Trace
    };
    log::log!(
        level,
        "Cycle {}, ctx={}, pc={}, instruction={:?}, stack={:?}",
        state.traces.clock(),
        state.registers.context,
        KERNEL.offset_name(pc),
        op,
        //state.stack(),
        0,
    );

    //assert!(pc < KERNEL.program.image.len(), "Kernel PC is out of range: {}", pc);
}

fn handle_error<F: Field>(state: &mut GenerationState<F>, err: ProgramError) -> anyhow::Result<()> {
    let exc_code: u8 = match err {
        ProgramError::OutOfGas => 0,
        ProgramError::InvalidOpcode => 1,
        ProgramError::StackUnderflow => 2,
        ProgramError::InvalidJumpDestination => 3,
        ProgramError::InvalidJumpiDestination => 4,
        ProgramError::StackOverflow => 5,
        _ => bail!("TODO: figure out what to do with this..."),
    };

    let checkpoint = state.checkpoint();

    let (row, _) = base_row(state);
    generate_exception(exc_code, state, row)
        .map_err(|_| anyhow::Error::msg("error handling errored..."))?;

    state
        .memory
        .apply_ops(state.traces.mem_ops_since(checkpoint.traces));
    Ok(())
}

pub(crate) fn transition<F: Field>(state: &mut GenerationState<F>) -> anyhow::Result<()> {
    let checkpoint = state.checkpoint();
    let result = try_perform_instruction(state);

    match result {
        Ok(()) => {
            state
                .memory
                .apply_ops(state.traces.mem_ops_since(checkpoint.traces));
            Ok(())
        }
        Err(e) => {
            if state.registers.is_kernel {
                let offset_name = KERNEL.offset_name(state.registers.program_counter);
                bail!(
                    "{:?} in kernel at pc={}, stack={:?}, memory={:?}",
                    e,
                    offset_name,
                    //state.stack(),
                    0,
                    state.memory.contexts[0].segments[Segment::KernelGeneral as usize].content,
                );
            }
            state.rollback(checkpoint);
            handle_error(state, e)
        }
    }
}
