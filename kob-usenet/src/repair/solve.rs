use std::collections::BTreeSet;
use std::fs::{File, OpenOptions};
use std::os::unix::fs::FileExt;

use crate::repair::field::FIELD;
use crate::repair::spool::{Row, Spool};
use crate::repair::{RepairError, Source, damaged_slices};
use crate::vendor::par2::{Id, Reader};

const MAX_WORKERS: usize = 8;
const BATCH: usize = 16;

#[allow(
    clippy::too_many_arguments,
    reason = "the solve is one pass over these inputs"
)]
pub fn solve(
    reader: &Reader,
    set: Id,
    spool: &Spool,
    rows: &[Row],
    missing: &[usize],
    sources: &[Source],
    memory: usize,
    cancelled: &(dyn Fn() -> bool + Sync),
) -> Result<(), RepairError> {
    let plan = reader.repair_set(set)?;
    let slice_size = plan.slice_size();
    let count = missing.len();
    let matrix = count.saturating_mul(count).saturating_mul(6);
    let available = memory.saturating_sub(matrix);
    let stripe = (available / (count + 2)).min(slice_size) & !3;
    if stripe == 0 {
        return Err(RepairError::Memory {
            need: count,
            required: matrix.saturating_add((count + 2) * 4),
            limit: memory,
        });
    }
    let batch = ((available - count * stripe) / stripe).clamp(1, BATCH);
    let workers = std::thread::available_parallelism()
        .map_or(1, |n| n.get() / 2)
        .clamp(1, MAX_WORKERS.min(batch));
    let chosen: Vec<Row> = select(missing, rows, cancelled)?
        .into_iter()
        .map(|i| rows[i])
        .collect();
    let inverse = invert(missing, &chosen, cancelled)?;
    let lost: BTreeSet<usize> = missing.iter().copied().collect();
    let touched: BTreeSet<usize> = missing
        .iter()
        .filter_map(|&index| plan.source(index).map(|(position, _)| position))
        .collect();
    let files = sources
        .iter()
        .enumerate()
        .map(|(position, source)| open(source, touched.contains(&position)))
        .collect::<Result<Vec<_>, RepairError>>()?;
    let present: Vec<usize> = plan
        .sources()
        .iter()
        .zip(sources)
        .filter(|(_, ours)| ours.present)
        .flat_map(|(source, _)| source.slices.clone())
        .filter(|index| !lost.contains(index))
        .collect();
    let mut accumulated = vec![0; count * stripe];
    let mut scratch = vec![0; batch * stripe];
    for offset in (0..slice_size).step_by(stripe) {
        let width = stripe.min(slice_size - offset);
        let mut lanes: Vec<(&mut [u8], Row)> = accumulated
            .chunks_mut(stripe)
            .zip(chosen.iter().copied())
            .collect();
        for (row, chosen) in &mut lanes {
            spool.read(*chosen, offset, &mut row[..width])?;
        }
        for group in present.chunks(batch) {
            if cancelled() {
                return Err(RepairError::Cancelled);
            }
            let mut inputs = Vec::with_capacity(group.len());
            for (&index, buf) in group.iter().zip(scratch.chunks_mut(stripe)) {
                let Some((position, range)) = plan.source(index) else {
                    continue;
                };
                let Some(n) = within(range.end - range.start, offset, width) else {
                    continue;
                };
                let file = files[position].as_ref().expect("present sources are open");
                file.read_exact_at(&mut buf[..n], range.start + offset as u64)?;
                inputs.push((index, &buf[..n]));
            }
            on_workers(&mut lanes, workers, |(row, chosen)| {
                for &(index, data) in &inputs {
                    let coefficient = FIELD.coefficient(index, chosen.exponent);
                    FIELD.add_scaled(
                        &mut row[..data.len().next_multiple_of(2)],
                        data,
                        coefficient,
                    );
                }
            });
        }
        if cancelled() {
            return Err(RepairError::Cancelled);
        }
        let solved: Vec<(&[u8], Row)> = lanes.iter().map(|(row, c)| (&row[..width], *c)).collect();
        let wanted: Vec<(usize, usize)> = missing.iter().copied().enumerate().collect();
        let groups = wanted.chunks(count.div_ceil(workers));
        std::thread::scope(|scope| -> Result<(), RepairError> {
            let handles: Vec<_> = groups
                .zip(scratch.chunks_mut(stripe))
                .map(|(group, out)| {
                    let (plan, files, solved, inverse) = (&plan, &files, &solved, &inverse);
                    scope.spawn(move || -> std::io::Result<()> {
                        for &(k, index) in group {
                            let Some((position, range)) = plan.source(index) else {
                                continue;
                            };
                            let Some(n) = within(range.end - range.start, offset, width) else {
                                continue;
                            };
                            let out = &mut out[..width];
                            out.fill(0);
                            for ((row, _), &coefficient) in
                                solved.iter().zip(&inverse[k * count..(k + 1) * count])
                            {
                                FIELD.add_scaled(out, row, coefficient);
                            }
                            let file = files[position].as_ref().expect("touched sources are open");
                            file.write_all_at(&out[..n], range.start + offset as u64)?;
                        }
                        Ok(())
                    })
                })
                .collect();
            for handle in handles {
                handle.join().expect("a repair worker panicked")?;
            }
            Ok(())
        })?;
    }
    for position in touched {
        let source = &sources[position];
        if !damaged_slices(
            &source.path,
            source.file.length,
            0,
            &[],
            &source.file.hash,
            &cancelled,
        )
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::Interrupted => RepairError::Cancelled,
            _ => RepairError::Io(e),
        })?
        .is_empty()
        {
            return Err(RepairError::Failed(format!(
                "{} still fails its checksum after repair",
                source.file.name
            )));
        }
    }
    Ok(())
}

fn on_workers<T: Send>(items: &mut [T], workers: usize, work: impl Fn(&mut T) + Sync) {
    if workers <= 1 || items.len() <= 1 {
        items.iter_mut().for_each(work);
        return;
    }
    let size = items.len().div_ceil(workers);
    std::thread::scope(|scope| {
        for group in items.chunks_mut(size) {
            let work = &work;
            scope.spawn(move || group.iter_mut().for_each(work));
        }
    });
}

fn within(length: u64, offset: usize, width: usize) -> Option<usize> {
    let left = length.checked_sub(offset as u64).filter(|&left| left > 0)?;
    Some(usize::try_from(left).map_or(width, |left| left.min(width)))
}

fn open(source: &Source, touched: bool) -> Result<Option<File>, RepairError> {
    if !touched {
        return Ok(source
            .present
            .then(|| File::open(&source.path))
            .transpose()?);
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&source.path)?;
    if file.metadata()?.len() != source.file.length {
        file.set_len(source.file.length)?;
    }
    Ok(Some(file))
}

fn select(
    missing: &[usize],
    rows: &[Row],
    cancelled: &(dyn Fn() -> bool + Sync),
) -> Result<Vec<usize>, RepairError> {
    let count = missing.len();
    let mut basis = vec![0u16; count * count];
    let mut pivots = Vec::with_capacity(count);
    let mut selected = Vec::with_capacity(count);
    let mut scratch = vec![0u16; count];
    for (index, row) in rows.iter().enumerate() {
        if selected.len() == count {
            break;
        }
        if cancelled() {
            return Err(RepairError::Cancelled);
        }
        for (entry, &input) in scratch.iter_mut().zip(missing) {
            *entry = FIELD.coefficient(input, row.exponent);
        }
        for (reduced, &pivot) in pivots.iter().enumerate() {
            let factor = scratch[pivot];
            for column in pivot..count {
                scratch[column] ^= FIELD.mul(factor, basis[reduced * count + column]);
            }
        }
        let Some(pivot) = scratch.iter().position(|&v| v != 0) else {
            continue;
        };
        let scale = FIELD.inverse(scratch[pivot]);
        for entry in &mut scratch {
            *entry = FIELD.mul(*entry, scale);
        }
        let start = selected.len() * count;
        basis[start..start + count].copy_from_slice(&scratch);
        pivots.push(pivot);
        selected.push(index);
    }
    if selected.len() < count {
        return Err(RepairError::Dependent);
    }
    Ok(selected)
}

fn invert(
    missing: &[usize],
    rows: &[Row],
    cancelled: &(dyn Fn() -> bool + Sync),
) -> Result<Vec<u16>, RepairError> {
    let count = missing.len();
    let width = count * 2;
    let mut matrix = vec![0u16; count * width];
    for (r, row) in rows.iter().enumerate() {
        for (column, &input) in missing.iter().enumerate() {
            matrix[r * width + column] = FIELD.coefficient(input, row.exponent);
        }
        matrix[r * width + count + r] = 1;
    }
    for column in 0..count {
        if cancelled() {
            return Err(RepairError::Cancelled);
        }
        let pivot = (column..count)
            .find(|&r| matrix[r * width + column] != 0)
            .ok_or(RepairError::Dependent)?;
        for entry in 0..width {
            matrix.swap(column * width + entry, pivot * width + entry);
        }
        let scale = FIELD.inverse(matrix[column * width + column]);
        for entry in 0..width {
            matrix[column * width + entry] = FIELD.mul(matrix[column * width + entry], scale);
        }
        for r in 0..count {
            let factor = matrix[r * width + column];
            if r == column || factor == 0 {
                continue;
            }
            for entry in 0..width {
                matrix[r * width + entry] ^= FIELD.mul(factor, matrix[column * width + entry]);
            }
        }
    }
    Ok(matrix
        .chunks(width)
        .flat_map(|row| row[count..].iter().copied())
        .collect())
}
