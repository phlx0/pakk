use std::io::{self, Write};
use std::mem;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::thread::{self, JoinHandle};

use rayon::prelude::*;

pub type Encode = Arc<dyn Fn(&[u8]) -> io::Result<Vec<u8>> + Send + Sync>;

pub struct ChunkedWriter<W> {
    buf: Vec<u8>,
    chunk_size: usize,
    batch: Vec<Vec<u8>>,
    batch_len: usize,
    tx: Option<SyncSender<Vec<Vec<u8>>>>,
    worker: Option<JoinHandle<io::Result<W>>>,
}

impl<W> std::fmt::Debug for ChunkedWriter<W> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChunkedWriter")
            .field("chunk_size", &self.chunk_size)
            .finish_non_exhaustive()
    }
}

impl<W: Write + Send + 'static> ChunkedWriter<W> {
    pub fn new(inner: W, chunk_size: usize, encode: Encode) -> Self {
        let (tx, rx) = sync_channel::<Vec<Vec<u8>>>(1);
        let worker = thread::spawn(move || encode_worker(inner, &rx, &encode));
        Self {
            buf: Vec::with_capacity(chunk_size),
            chunk_size,
            batch: Vec::new(),
            batch_len: rayon::current_num_threads().max(1) * 2,
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
        let chunk = mem::replace(&mut self.buf, Vec::with_capacity(self.chunk_size));
        self.batch.push(chunk);
        if self.batch.len() >= self.batch_len {
            self.send_batch()?;
        }
        Ok(())
    }

    fn send_batch(&mut self) -> io::Result<()> {
        let batch = mem::take(&mut self.batch);
        if self.tx.as_ref().is_some_and(|tx| tx.send(batch).is_ok()) {
            return Ok(());
        }
        drop(self.tx.take());
        Err(self
            .join()
            .err()
            .unwrap_or_else(|| io::Error::other("compression worker stopped")))
    }

    fn join(&mut self) -> io::Result<W> {
        self.worker
            .take()
            .ok_or_else(|| io::Error::other("writer already finished"))?
            .join()
            .map_err(|_| io::Error::other("compression worker panicked"))?
    }
}

impl<W: Write + Send + 'static> Write for ChunkedWriter<W> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        let n = data.len().min(self.chunk_size - self.buf.len());
        self.buf.extend_from_slice(&data[..n]);
        if self.buf.len() == self.chunk_size {
            self.push_chunk()?;
        }
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn encode_worker<W: Write>(
    mut out: W,
    rx: &Receiver<Vec<Vec<u8>>>,
    encode: &Encode,
) -> io::Result<W> {
    let mut wrote_any = false;
    for batch in rx {
        let encoded: Vec<io::Result<Vec<u8>>> = batch.par_iter().map(|c| encode(c)).collect();
        for chunk in encoded {
            out.write_all(&chunk?)?;
            wrote_any = true;
        }
    }
    if !wrote_any {
        out.write_all(&encode(&[])?)?;
    }
    out.flush()?;
    Ok(out)
}
