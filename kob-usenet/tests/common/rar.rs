use kob_usenet::vendor::yenc::crc32;

const SIGNATURE: &[u8] = b"Rar!\x1a\x07\x01\x00";
const MAIN: u64 = 1;
const FILE: u64 = 2;
const END: u64 = 5;
const HAS_DATA: u64 = 0x2;
const SPLIT_BEFORE: u64 = 0x8;
const SPLIT_AFTER: u64 = 0x10;
const MULTI_VOLUME: u64 = 0x1;
const NUMBERED: u64 = 0x2;
const MORE_VOLUMES: u64 = 0x1;
const UNIX: u64 = 1;
const DATA_CRC: u64 = 0x4;

pub struct Member<'a> {
    pub name: &'a str,
    pub data: &'a [u8],
}

pub struct Piece {
    pub volume: usize,
    pub at: usize,
    pub len: usize,
}

pub struct Set {
    pub volumes: Vec<Vec<u8>>,
    pub pieces: Vec<Vec<Piece>>,
}

pub fn volume_name(set: &str, volume: usize, volumes: usize) -> String {
    if volumes == 1 {
        format!("{set}.rar")
    } else {
        format!("{set}.part{:02}.rar", volume + 1)
    }
}

pub fn stored(members: &[Member<'_>], capacity: usize) -> Set {
    let multi = members.iter().map(|m| m.data.len()).sum::<usize>() > capacity;
    let mut volumes = vec![open(0, multi)];
    let mut pieces: Vec<Vec<Piece>> = members.iter().map(|_| Vec::new()).collect();
    let mut room = capacity;
    for (m, member) in members.iter().enumerate() {
        let mut done = 0;
        while done < member.data.len() {
            if room == 0 {
                let last = volumes.last_mut().unwrap();
                last.extend(block(END, 0, &[], &vint(MORE_VOLUMES)));
                volumes.push(open(volumes.len(), multi));
                room = capacity;
            }
            let len = room.min(member.data.len() - done);
            let mut flags = 0;
            if done > 0 {
                flags |= SPLIT_BEFORE;
            }
            if done + len < member.data.len() {
                flags |= SPLIT_AFTER;
            }
            let number = volumes.len() - 1;
            let volume = &mut volumes[number];
            volume.extend(file_header(member, flags, len));
            pieces[m].push(Piece {
                volume: number,
                at: volume.len(),
                len,
            });
            volume.extend_from_slice(&member.data[done..done + len]);
            done += len;
            room -= len;
        }
    }
    volumes
        .last_mut()
        .unwrap()
        .extend(block(END, 0, &[], &vint(0)));
    Set { volumes, pieces }
}

fn open(number: usize, multi: bool) -> Vec<u8> {
    let mut fields = Vec::new();
    match (multi, number) {
        (false, _) => fields.extend(vint(0)),
        (true, 0) => fields.extend(vint(MULTI_VOLUME)),
        (true, n) => {
            fields.extend(vint(MULTI_VOLUME | NUMBERED));
            fields.extend(vint(n as u64));
        }
    }
    [SIGNATURE.to_vec(), block(MAIN, 0, &[], &fields)].concat()
}

fn file_header(member: &Member<'_>, flags: u64, data_len: usize) -> Vec<u8> {
    let mut fields = Vec::new();
    fields.extend(vint(DATA_CRC));
    fields.extend(vint(member.data.len() as u64));
    fields.extend(vint(0o644));
    fields.extend(crc32(0, member.data).to_le_bytes());
    fields.extend(vint(0));
    fields.extend(vint(UNIX));
    fields.extend(vint(member.name.len() as u64));
    fields.extend_from_slice(member.name.as_bytes());
    block(FILE, flags | HAS_DATA, &vint(data_len as u64), &fields)
}

fn block(kind: u64, flags: u64, data_len: &[u8], fields: &[u8]) -> Vec<u8> {
    let body = [vint(kind), vint(flags), data_len.to_vec(), fields.to_vec()].concat();
    let sized = [vint(body.len() as u64), body].concat();
    [crc32(0, &sized).to_le_bytes().to_vec(), sized].concat()
}

fn vint(mut value: u64) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return out;
        }
        out.push(byte | 0x80);
    }
}
