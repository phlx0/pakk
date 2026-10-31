use std::io::{self, BufRead, BufReader, Cursor, Read, Write};
use std::mem;
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::thread::{self, JoinHandle};

use flate2::Compression;
use flate2::bufread::{DeflateDecoder, MultiGzDecoder};
use flate2::write::DeflateEncoder;
use rayon::prelude::*;

pub const CHUNK_SIZE: usize = 1 << 20;
const MAX_MEMBER_SIZE: usize = 64 << 20;
const HEADER_LEN: usize = 20;
const TRAILER_LEN: usize = 8;
const SUBFIELD_ID: [u8; 2] = *b"PZ";

fn batch_len() -> usize {
    rayon::current_num_threads().max(1) * 2
}

#[derive(Debug)]
pub struct ParGzWriter<W> {
    buf: Vec<u8>,
    batch: Vec<Vec<u8>>,
    batch_len: usize,
    tx: Option<SyncSender<Vec<Vec<u8>>>>,
    worker: Option<JoinHandle<io::Result<W>>>,
}

impl<W: Write + Send + 'static> ParGzWriter<W> {
    pub fn new(inner: W, level: u32) -> Self {
        let (tx, rx) = sync_channel::<Vec<Vec<u8>>>(1);
        let worker = thread::spawn(move || compress_worker(inner, &rx, level));
        Self {
            buf: Vec::with_capacity(CHUNK_SIZE),
            batch: Vec::new(),
            batch_len: batch_len(),
            tx: Some(tx),
            worker: Some(worker),
        }
    }

    pub fn finish(mut self) -> io::Result<W> {
        if !self.buf.is_empty() {
            self.push_chunk()?;
        }
        if !self.batch.is_empty() {
            self.send_batch()?;
        }
        drop(self.tx.take());
        self.join()
    }

    fn push_chunk(&mut self) -> io::Result<()> {
        let chunk = mem::replace(&mut self.buf, Vec::with_capacity(CHUNK_SIZE));
        self.batch.push(chunk);
        if self.batch.len() >= self.batch_len {
            self.send_batch()?;
        }
        Ok(())
    }

    fn send_batch(&mut self) -> io::Result<()> {
        let batch = mem::take(&mut self.batch);
        let sent = self.tx.as_ref().is_some_and(|tx| tx.send(batch).is_ok());
        if sent {
            Ok(())
        } else {
            drop(self.tx.take());
            Err(self
                .join()
                .err()
                .unwrap_or_else(|| io::Error::other("gzip worker stopped")))
        }
    }

    fn join(&mut self) -> io::Result<W> {
        self.worker
            .take()
            .ok_or_else(|| io::Error::other("gzip writer already finished"))?
            .join()
            .map_err(|_| io::Error::other("gzip worker panicked"))?
    }
}

impl<W: Write + Send + 'static> Write for ParGzWriter<W> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        let n = data.len().min(CHUNK_SIZE - self.buf.len());
        self.buf.extend_from_slice(&data[..n]);
        if self.buf.len() == CHUNK_SIZE {
            self.push_chunk()?;
        }
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn compress_worker<W: Write>(mut out: W, rx: &Receiver<Vec<Vec<u8>>>, level: u32) -> io::Result<W> {
    let mut wrote_any = false;
    for batch in rx {
        let members: Vec<io::Result<Vec<u8>>> = batch
            .par_iter()
            .map(|chunk| encode_member(chunk, level))
            .collect();
        for member in members {
            out.write_all(&member?)?;
            wrote_any = true;
        }
    }
    if !wrote_any {
        out.write_all(&encode_member(&[], level)?)?;
    }
    out.flush()?;
    Ok(out)
}

pub fn encode_member(data: &[u8], level: u32) -> io::Result<Vec<u8>> {
    let mut out = Vec::with_capacity(data.len() / 2 + 64);
    out.extend_from_slice(&[0x1f, 0x8b, 8, 0x04, 0, 0, 0, 0, 0, 0xff]);
    out.extend_from_slice(&8u16.to_le_bytes());
    out.extend_from_slice(&SUBFIELD_ID);
    out.extend_from_slice(&4u16.to_le_bytes());
    out.extend_from_slice(&[0; 4]);

    let mut encoder = DeflateEncoder::new(out, Compression::new(level));
    encoder.write_all(data)?;
    let mut out = encoder.finish()?;

    out.extend_from_slice(&crc32fast::hash(data).to_le_bytes());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    let total = u32::try_from(out.len()).map_err(|_| io::Error::other("gzip member too large"))?;
    out[16..20].copy_from_slice(&total.to_le_bytes());
    Ok(out)
}

fn member_size(header: &[u8]) -> Option<usize> {
    let tagged = header.len() >= HEADER_LEN
        && header[..4] == [0x1f, 0x8b, 8, 0x04]
        && header[10..16] == [8, 0, SUBFIELD_ID[0], SUBFIELD_ID[1], 4, 0];
    if !tagged {
        return None;
    }
    let size = u32::from_le_bytes(header[16..20].try_into().ok()?) as usize;
    (HEADER_LEN + TRAILER_LEN..=MAX_MEMBER_SIZE)
        .contains(&size)
        .then_some(size)
}

pub fn decoder<R: Read + Send + 'static>(reader: R) -> io::Result<Box<dyn Read + Send>> {
    let mut reader = BufReader::with_capacity(CHUNK_SIZE, reader);
    if member_size(reader.fill_buf()?).is_some() {
        Ok(Box::new(ParGzReader::new(reader)))
    } else {
        Ok(Box::new(MultiGzDecoder::new(reader)))
    }
}

#[derive(Debug)]
pub struct ParGzReader {
    current: Cursor<Vec<u8>>,
    rx: Option<Receiver<io::Result<Vec<u8>>>>,
    worker: Option<JoinHandle<()>>,
}

impl ParGzReader {
    pub fn new<R: Read + Send + 'static>(reader: R) -> Self {
        let (tx, rx) = sync_channel(batch_len() * 2);
        let worker = thread::spawn(move || {
            if let Err(err) = decompress_worker(reader, &tx) {
                let _ = tx.send(Err(err));
            }
        });
        Self {
            current: Cursor::new(Vec::new()),
            rx: Some(rx),
            worker: Some(worker),
        }
    }
}

impl Read for ParGzReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            let n = self.current.read(buf)?;
            if n > 0 || buf.is_empty() {
                return Ok(n);
            }
            let Some(rx) = &self.rx else { return Ok(0) };
            if let Ok(next) = rx.recv() {
                self.current = Cursor::new(next?);
            } else {
                self.rx = None;
                if let Some(worker) = self.worker.take() {
                    worker
                        .join()
                        .map_err(|_| io::Error::other("gzip worker panicked"))?;
                }
                return Ok(0);
            }
        }
    }
}

fn decompress_worker<R: Read>(
    mut reader: R,
    tx: &SyncSender<io::Result<Vec<u8>>>,
) -> io::Result<()> {
    loop {
        let mut members = Vec::with_capacity(batch_len());
        while members.len() < members.capacity() {
            match read_member(&mut reader)? {
                Some(member) => members.push(member),
                None => break,
            }
        }
        if members.is_empty() {
            return Ok(());
        }
        let decoded: Vec<io::Result<Vec<u8>>> =
            members.par_iter().map(|m| decode_member(m)).collect();
        for chunk in decoded {
            if tx.send(chunk).is_err() {
                return Ok(());
            }
        }
    }
}

fn read_member<R: Read>(reader: &mut R) -> io::Result<Option<Vec<u8>>> {
    let mut header = [0u8; HEADER_LEN];
    let mut filled = 0;
    while filled < HEADER_LEN {
        match reader.read(&mut header[filled..])? {
            0 if filled == 0 => return Ok(None),
            0 => return Err(invalid("truncated gzip member header")),
            n => filled += n,
        }
    }
    let size = member_size(&header)
        .ok_or_else(|| invalid("gzip stream mixes pakk and foreign members"))?;
    let mut member = Vec::with_capacity(size);
    member.extend_from_slice(&header);
    reader
        .take((size - HEADER_LEN) as u64)
        .read_to_end(&mut member)?;
    if member.len() != size {
        return Err(invalid("truncated gzip member"));
    }
    Ok(Some(member))
}

fn decode_member(member: &[u8]) -> io::Result<Vec<u8>> {
    let (body, trailer) = member[HEADER_LEN..].split_at(member.len() - HEADER_LEN - TRAILER_LEN);
    let crc = u32::from_le_bytes(trailer[..4].try_into().expect("4 bytes"));
    let len = u32::from_le_bytes(trailer[4..].try_into().expect("4 bytes")) as usize;

    if len > MAX_MEMBER_SIZE {
        return Err(invalid(
            "gzip block is larger than pakk ever writes, archive is corrupt",
        ));
    }
    let mut out = Vec::with_capacity(len);
    DeflateDecoder::new(body)
        .take(len as u64 + 1)
        .read_to_end(&mut out)?;
    if out.len() != len || crc32fast::hash(&out) != crc {
        return Err(invalid("gzip checksum mismatch, archive is corrupt"));
    }
    Ok(out)
}

fn invalid(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(len: usize) -> Vec<u8> {
        (0..len).map(|i| ((i * 7) ^ (i >> 9)) as u8).collect()
    }

    fn compress(data: &[u8]) -> Vec<u8> {
        let mut writer = ParGzWriter::new(Vec::new(), 6);
        for piece in data.chunks(7919) {
            writer.write_all(piece).unwrap();
        }
        writer.finish().unwrap()
    }

    fn decompress(gz: Vec<u8>) -> io::Result<Vec<u8>> {
        let mut out = Vec::new();
        decoder(Cursor::new(gz))?.read_to_end(&mut out)?;
        Ok(out)
    }

    #[test]
    fn roundtrips_across_chunk_boundaries() {
        for len in [0, 1, CHUNK_SIZE - 1, CHUNK_SIZE, 3 * CHUNK_SIZE + 17] {
            let data = sample(len);
            assert_eq!(decompress(compress(&data)).unwrap(), data, "len {len}");
        }
    }

    #[test]
    fn output_is_standard_gzip() {
        let data = sample(2 * CHUNK_SIZE + 5);
        let mut out = Vec::new();
        MultiGzDecoder::new(&compress(&data)[..])
            .read_to_end(&mut out)
            .unwrap();
        assert_eq!(out, data);
    }

    #[test]
    fn reads_foreign_gzip() {
        let data = sample(100_000);
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), Compression::default());
        enc.write_all(&data).unwrap();
        assert_eq!(decompress(enc.finish().unwrap()).unwrap(), data);
    }

    #[test]
    fn detects_corruption() {
        let mut gz = compress(&sample(CHUNK_SIZE));
        let mid = gz.len() / 2;
        gz[mid] ^= 0xff;
        assert!(decompress(gz).is_err());
    }

    #[test]
    fn rejects_decompression_bomb_without_inflating_it() {
        let mut member = encode_member(&vec![0; 16 << 20], 9).unwrap();
        let isize_at = member.len() - 4;
        member[isize_at..].copy_from_slice(&16u32.to_le_bytes());
        let err = decompress(member).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);

        let mut member = encode_member(&[1, 2, 3], 9).unwrap();
        let isize_at = member.len() - 4;
        member[isize_at..].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(decompress(member).is_err());
    }

    #[test]
    fn detects_truncation() {
        let mut gz = compress(&sample(2 * CHUNK_SIZE));
        gz.truncate(gz.len() - 3);
        assert!(decompress(gz).is_err());
    }
}
