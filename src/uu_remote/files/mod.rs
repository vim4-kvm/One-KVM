//! Authenticated UU FILE RPCs operate on board shares, never the HDMI host's disks.
mod protocol;
pub mod storage;
#[cfg(test)]
mod tests;

use super::wire;
use protocol::*;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use std::time::Duration;
use storage::{Access, Area, Location, Reservation, Storage, FREE_RESERVE, MAX_FILE_BYTES};
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

const IDLE: Duration = Duration::from_secs(120);
const MAX_TASKS: usize = 4;

struct UploadFile {
    file: File,
    position: u64,
    next_block: Option<u64>,
    last_block: Option<(u64, Vec<u8>)>,
}
struct Upload {
    access: Access,
    files: Vec<(FileInfo, Location)>,
    strategy: u64,
    open: HashMap<u64, UploadFile>,
    finished: HashSet<u64>,
    reservation: Reservation,
    updated: Instant,
}
struct Reply {
    case: u32,
    body: Vec<u8>,
}

pub struct Connection {
    storage: Arc<Storage>,
    closed: CancellationToken,
    sequence: Arc<AtomicU64>,
    request_id: AtomicU64,
    uploads: Mutex<HashMap<u64, Upload>>,
    downloads: parking_lot::Mutex<HashMap<u64, CancellationToken>>,
    pending: parking_lot::Mutex<HashMap<u64, oneshot::Sender<Reply>>>,
    outgoing: mpsc::Sender<Vec<u8>>,
    receiver: parking_lot::Mutex<Option<mpsc::Receiver<Vec<u8>>>>,
}

impl Connection {
    pub fn new(
        storage: Arc<Storage>,
        sequence: Arc<AtomicU64>,
        closed: CancellationToken,
    ) -> Arc<Self> {
        let (outgoing, receiver) = mpsc::channel(32);
        let connection = Arc::new(Self {
            storage,
            closed,
            sequence,
            request_id: AtomicU64::new(1),
            uploads: Mutex::new(HashMap::new()),
            downloads: Default::default(),
            pending: Default::default(),
            outgoing,
            receiver: parking_lot::Mutex::new(Some(receiver)),
        });
        let weak = Arc::downgrade(&connection);
        let closed = connection.closed.clone();
        tokio::spawn(async move {
            let mut timer = tokio::time::interval(Duration::from_secs(10));
            loop {
                tokio::select! {
                    _=closed.cancelled()=>{
                        if let Some(connection)=weak.upgrade() {connection.close().await;}
                        break;
                    },
                    _=timer.tick()=>{}
                }
                let Some(connection) = weak.upgrade() else {
                    break;
                };
                connection
                    .uploads
                    .lock()
                    .await
                    .retain(|_, task| task.updated.elapsed() < IDLE);
            }
        });
        connection
    }

    pub fn take_receiver(&self) -> Option<mpsc::Receiver<Vec<u8>>> {
        self.receiver.lock().take()
    }
    pub async fn cancelled(&self) {
        self.closed.cancelled().await;
    }
    pub async fn close(&self) {
        self.closed.cancel();
        self.uploads.lock().await.clear();
        for (_, token) in self.downloads.lock().drain() {
            token.cancel();
        }
        self.pending.lock().clear();
    }

    async fn send(
        &self,
        request_id: u64,
        response: bool,
        case: u32,
        body: &[u8],
    ) -> Result<(), u64> {
        let bytes = rpc(
            self.sequence.fetch_add(1, Ordering::Relaxed),
            request_id,
            response,
            case,
            body,
        );
        if bytes.len() > MAX_WIRE_BYTES {
            return Err(PROTOCOL);
        }
        tokio::select! {
            biased;
            _=self.closed.cancelled()=>Err(CANCELED),
            result=tokio::time::timeout(Duration::from_secs(10),self.outgoing.send(bytes))=>{
                match result {Ok(Ok(()))=>Ok(()),_=>Err(IO)}
            }
        }
    }

    async fn request(
        &self,
        case: u32,
        body: &[u8],
        cancel: &CancellationToken,
    ) -> Result<Reply, u64> {
        let id = self.request_id.fetch_add(1, Ordering::Relaxed);
        let (send, receive) = oneshot::channel();
        self.pending.lock().insert(id, send);
        let exchange = async {
            self.send(id, false, case, body).await?;
            receive.await.map_err(|_| CANCELED)
        };
        let result = tokio::select! {
            biased;
            _=cancel.cancelled()=>Err(CANCELED),
            _=self.closed.cancelled()=>Err(CANCELED),
            reply=tokio::time::timeout(Duration::from_secs(20),exchange)=>match reply {Ok(reply)=>reply,Err(_)=>Err(IO)},
        };
        self.pending.lock().remove(&id);
        result
    }

    /// Requests may arrive on CONTROL or FILE. Every FTP reply goes to FILE.
    pub async fn handle(self: &Arc<Self>, data: &[u8]) -> bool {
        let message = match parse(data) {
            Ok(Some(message)) => message,
            _ => return false,
        };
        if self.closed.is_cancelled() {
            return true;
        }
        if message.response {
            if let Some(waiter) = self.pending.lock().remove(&message.request_id) {
                let _ = waiter.send(Reply {
                    case: message.case,
                    body: message.body.to_vec(),
                });
            }
            return true;
        }
        let fields = match wire::decode(message.body) {
            Ok(fields) => fields,
            Err(_) => return true,
        };
        let result = match message.case {
            1 => self.receive(message.request_id, &fields).await,
            2 => self.download(message.request_id, &fields).await,
            3 => self.open_upload(message.request_id, &fields).await,
            4 => self.write_block(message.request_id, &fields).await,
            9 => self.complete(message.request_id, &fields).await,
            10 => self.directory(message.request_id, &fields).await,
            11 => self.exists(message.request_id, &fields).await,
            6 => self.mkdir(message.request_id, &fields).await,
            12 => {
                self.send(message.request_id, true, 5, &status(Vec::new(), 1, OK))
                    .await
            }
            _ => Err(PERMISSION),
        };
        if let Err(code) = result {
            let (case, error_field) = match message.case {
                1 => (1, 3),
                2 => (2, 2),
                3 => (3, 3),
                4 => (4, 3),
                8 | 9 => (6, 2),
                10 => (7, 4),
                _ => (5, 1),
            };
            let mut body = if [1, 2, 3, 4, 8, 9, 10].contains(&message.case) {
                Id::parse(&fields).map(Id::payload).unwrap_or_default()
            } else {
                Vec::new()
            };
            if message.case == 4 {
                wire::put_varint(2, number(&fields, 2), &mut body);
            }
            wire::put_varint(error_field, code, &mut body);
            let _ = self.send(message.request_id, true, case, &body).await;
        }
        true
    }

    async fn directory(&self, request: u64, fields: &[wire::WireField<'_>]) -> Result<(), u64> {
        let id = Id::parse(fields)?;
        let path = text(fields, 2)?;
        let mut entries = Vec::new();
        if let Some(location) = Location::parse(path).map_err(|e| error(&e))? {
            let access = self
                .storage
                .access(&location)
                .await
                .map_err(|e| error(&e))?;
            for entry in access.entries(&location).map_err(|e| error(&e))? {
                let target = location.child(&entry.name).map_err(|e| error(&e))?;
                entries.push(entry_payload(
                    if entry.directory { 0 } else { 4 },
                    &entry.name,
                    entry.size,
                    entry.modified,
                    &target.display(),
                ));
            }
        } else {
            entries.push(entry_payload(3, "板子文件（非被控机磁盘）", 0, 0, "/files"));
            entries.push(entry_payload(
                3,
                "虚拟盘镜像（先卸载介质）",
                0,
                0,
                "/images",
            ));
        }
        let mut list = Vec::new();
        for entry in &entries {
            wire::put_bytes(1, entry, &mut list);
        }
        let compressed = compress(&list)?;
        let mut body = status(id.payload(), 4, OK);
        wire::put_bytes(2, path.as_bytes(), &mut body);
        wire::put_bytes(5, &compressed, &mut body);
        // LIST_COMPRESS viewers read dir_data. Keep small inline listings for older clients.
        if body.len() + list.len() < MAX_WIRE_BYTES - 256 {
            for entry in entries {
                wire::put_bytes(3, &entry, &mut body);
            }
        }
        self.send(request, true, 7, &body).await
    }

    async fn receive(&self, request: u64, fields: &[wire::WireField<'_>]) -> Result<(), u64> {
        let id = Id::parse(fields)?;
        let location = Location::parse(text(fields, 2)?)
            .map_err(|e| error(&e))?
            .ok_or(PERMISSION)?;
        let access = self
            .storage
            .access(&location)
            .await
            .map_err(|e| error(&e))?;
        access
            .open_directory(&location.parts)
            .map_err(|e| error(&e))?;
        let files = file_list(fields)?;
        let strategy = number(fields, 4);
        if strategy > 4 {
            return Err(PROTOCOL);
        }
        let folder = text(fields, 5)?.replace('\\', "/");
        let target = if folder.is_empty() {
            location.clone()
        } else {
            location.child(&folder).map_err(|e| error(&e))?
        };
        let mut paths = HashSet::new();
        let mut total = 0u64;
        let mut manifest = Vec::new();
        for file in files {
            if location.area == Area::Images && file.size < 512 {
                return Err(PROTOCOL);
            }
            total = total
                .checked_add(file.size)
                .filter(|n| *n <= MAX_FILE_BYTES)
                .ok_or(PROTOCOL)?;
            let name = file.path.replace('\\', "/");
            let path = if !folder.is_empty() && name.starts_with(&format!("{folder}/")) {
                location.child(&name)
            } else {
                target.child(&name)
            }
            .map_err(|e| error(&e))?;
            if !paths.insert(path.display()) {
                return Err(PROTOCOL);
            }
            manifest.push((file, path));
        }
        let reservation = self
            .storage
            .reserve(&access, total)
            .map_err(|e| error(&e))?;
        let mut uploads = self.uploads.lock().await;
        if uploads.len() >= MAX_TASKS || uploads.contains_key(&id.task) {
            return Err(PROTOCOL);
        }
        if !folder.is_empty() {
            access.mkdir(&target.parts).map_err(|e| error(&e))?;
        }
        let mut reply = status(id.payload(), 3, OK);
        for (file, _) in &manifest {
            wire::put_bytes(2, &file.encode(), &mut reply);
        }
        if !manifest.is_empty() {
            uploads.insert(
                id.task,
                Upload {
                    access,
                    files: manifest,
                    strategy,
                    open: HashMap::new(),
                    finished: HashSet::new(),
                    reservation,
                    updated: Instant::now(),
                },
            );
        }
        drop(uploads);
        self.send(request, true, 1, &reply).await
    }

    async fn open_upload(&self, request: u64, fields: &[wire::WireField<'_>]) -> Result<(), u64> {
        let id = Id::parse(fields)?;
        let mut tasks = self.uploads.lock().await;
        let task = tasks.get_mut(&id.task).ok_or(PROTOCOL)?;
        task.updated = Instant::now();
        let slot = usize::try_from(id.file)
            .ok()
            .and_then(|n| n.checked_sub(1))
            .ok_or(PROTOCOL)?;
        let (info, location) = task.files.get(slot).ok_or(PROTOCOL)?;
        if info.size != number(fields, 3)
            || task.open.contains_key(&id.file)
            || task.finished.contains(&id.file)
            || task.open.len() >= MAX_TASKS
        {
            return Err(PROTOCOL);
        }
        let existing = task.access.open_file(location, false);
        let mut reply = status(id.payload(), 3, OK);
        if existing.is_ok() && task.strategy == 3 {
            task.finished.insert(id.file);
            task.reservation.consume(info.size);
            wire::put_varint(2, 1, &mut reply);
        } else {
            let previous = if task.strategy == 2 {
                drop(existing);
                0
            } else {
                match existing {
                    Ok(file) => file.metadata().map_err(|e| error(&e))?.len(),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
                    Err(e) => return Err(error(&e)),
                }
            };
            if task.strategy == 4 && previous > info.size {
                return Err(PROTOCOL);
            }
            let resume = if task.strategy == 4 && previous <= info.size {
                previous
            } else {
                0
            };
            let (mut file, target) = if task.strategy == 2 {
                duplicate(&task.access, location)?
            } else {
                (
                    task.access
                        .open_file(location, true)
                        .map_err(|e| error(&e))?,
                    location.clone(),
                )
            };
            task.access
                .check_unmounted(&target, &file)
                .await
                .map_err(|e| error(&e))?;
            if resume == 0 {
                file.set_len(0).map_err(|e| error(&e))?;
            }
            file.seek(SeekFrom::Start(resume)).map_err(|e| error(&e))?;
            task.reservation.consume(resume);
            task.open.insert(
                id.file,
                UploadFile {
                    file,
                    position: resume,
                    next_block: None,
                    last_block: None,
                },
            );
            wire::put_varint(4, resume, &mut reply);
        }
        if task.finished.len() == task.files.len() {
            tasks.remove(&id.task);
        }
        drop(tasks);
        self.send(request, true, 3, &reply).await
    }

    async fn write_block(&self, request: u64, fields: &[wire::WireField<'_>]) -> Result<(), u64> {
        let id = Id::parse(fields)?;
        let block = number(fields, 2);
        let data = bytes(fields, 3);
        if data.is_empty() || data.len() > BLOCK_BYTES {
            return Err(PROTOCOL);
        }
        let mut tasks = self.uploads.lock().await;
        let task = tasks.get_mut(&id.task).ok_or(PROTOCOL)?;
        task.updated = Instant::now();
        let slot = usize::try_from(id.file)
            .ok()
            .and_then(|n| n.checked_sub(1))
            .ok_or(PROTOCOL)?;
        let expected = task.files.get(slot).ok_or(PROTOCOL)?.0.size;
        let upload = task.open.get_mut(&id.file).ok_or(PROTOCOL)?;
        let duplicate = upload
            .last_block
            .as_ref()
            .is_some_and(|(id, old)| *id == block && old.as_slice() == data);
        if !duplicate {
            if upload.next_block.is_some_and(|next| next != block)
                || (upload.next_block.is_none()
                    && ![
                        upload.position / BLOCK_BYTES as u64,
                        upload.position / BLOCK_BYTES as u64 + 1,
                    ]
                    .contains(&block))
                || upload
                    .position
                    .checked_add(data.len() as u64)
                    .is_none_or(|n| n > expected)
            {
                return Err(PROTOCOL);
            }
            if task.access.space().map_err(|e| error(&e))? < FREE_RESERVE + data.len() as u64 {
                return Err(DISK);
            }
            upload.file.write_all(data).map_err(|e| error(&e))?;
            upload.position += data.len() as u64;
            upload.next_block = block.checked_add(1);
            upload.last_block = Some((block, data.to_vec()));
            task.reservation.consume(data.len() as u64);
        }
        drop(tasks);
        let mut reply = status(id.payload(), 3, OK);
        wire::put_varint(2, block, &mut reply);
        wire::put_varint(4, data.len() as u64, &mut reply);
        self.send(request, true, 4, &reply).await
    }

    async fn complete(&self, request: u64, fields: &[wire::WireField<'_>]) -> Result<(), u64> {
        let id = Id::parse(fields)?;
        let failure = number(fields, 2);
        let canceled = ![0, OK].contains(&failure);
        let mut tasks = self.uploads.lock().await;
        if canceled || id.file == 0 || id.file == u64::MAX {
            tasks.remove(&id.task);
            if let Some(cancel) = self.downloads.lock().get(&id.task) {
                cancel.cancel();
            }
        } else {
            let task = tasks.get_mut(&id.task).ok_or(PROTOCOL)?;
            let slot = usize::try_from(id.file)
                .ok()
                .and_then(|n| n.checked_sub(1))
                .ok_or(PROTOCOL)?;
            let upload = task.open.remove(&id.file).ok_or(PROTOCOL)?;
            if upload.position != task.files.get(slot).ok_or(PROTOCOL)?.0.size {
                return Err(IO);
            }
            upload.file.sync_all().map_err(|e| error(&e))?;
            task.finished.insert(id.file);
            if task.finished.len() == task.files.len() {
                tasks.remove(&id.task);
            }
        }
        drop(tasks);
        self.send(
            request,
            true,
            6,
            &status(id.payload(), 2, if canceled { failure } else { OK }),
        )
        .await
    }

    async fn exists(&self, request: u64, fields: &[wire::WireField<'_>]) -> Result<(), u64> {
        let path = text(fields, 1)?;
        let location = Location::parse(path)
            .map_err(|e| error(&e))?
            .ok_or(PERMISSION)?;
        let access = self
            .storage
            .access(&location)
            .await
            .map_err(|e| error(&e))?;
        let mut reply = Vec::new();
        wire::put_bytes(1, path.as_bytes(), &mut reply);
        for (index, name) in repeated(fields, 2).enumerate() {
            if index >= MAX_FILES {
                return Err(PROTOCOL);
            }
            let name = std::str::from_utf8(name).map_err(|_| PROTOCOL)?;
            let child = location.child(name).map_err(|e| error(&e))?;
            let exists = access.open_file(&child, false).is_ok()
                || access.open_directory(&child.parts).is_ok();
            let mut entry = Vec::new();
            wire::put_bytes(1, name.as_bytes(), &mut entry);
            wire::put_varint(2, u64::from(exists), &mut entry);
            wire::put_bytes(2, &entry, &mut reply);
        }
        self.send(request, true, 8, &reply).await
    }

    async fn mkdir(&self, request: u64, fields: &[wire::WireField<'_>]) -> Result<(), u64> {
        let location = Location::parse(text(fields, 1)?)
            .map_err(|e| error(&e))?
            .ok_or(PERMISSION)?;
        let access = self
            .storage
            .access(&location)
            .await
            .map_err(|e| error(&e))?;
        access.mkdir(&location.parts).map_err(|e| error(&e))?;
        self.send(request, true, 5, &status(Vec::new(), 1, OK))
            .await
    }

    async fn download(
        self: &Arc<Self>,
        request: u64,
        fields: &[wire::WireField<'_>],
    ) -> Result<(), u64> {
        let id = Id::parse(fields)?;
        let location = Location::parse(text(fields, 2)?)
            .map_err(|e| error(&e))?
            .ok_or(PERMISSION)?;
        let access = self
            .storage
            .access(&location)
            .await
            .map_err(|e| error(&e))?;
        let folder = access.open_directory(&location.parts).is_ok();
        let mut files = Vec::new();
        if folder {
            collect(&access, &location, "", &mut files)?;
        } else {
            let file = access.open_file(&location, false).map_err(|e| error(&e))?;
            let meta = file.metadata().map_err(|e| error(&e))?;
            files.push((
                FileInfo {
                    path: location.parts.last().ok_or(PERMISSION)?.clone(),
                    size: meta.len(),
                    modified: modified(&meta),
                },
                location.clone(),
            ));
        }
        let mut total = 0u64;
        for (file, _) in &files {
            total = total
                .checked_add(file.size)
                .filter(|n| *n <= MAX_FILE_BYTES)
                .ok_or(PROTOCOL)?;
        }
        let mut reply = status(id.payload(), 2, OK);
        let infos: Vec<_> = files.iter().map(|(f, _)| f.clone()).collect();
        wire::put_bytes(5, &listing(&infos)?, &mut reply);
        if folder {
            wire::put_bytes(
                4,
                location
                    .parts
                    .last()
                    .map(String::as_str)
                    .unwrap_or(if location.area == Area::Files {
                        "files"
                    } else {
                        "images"
                    })
                    .as_bytes(),
                &mut reply,
            );
        }
        let inline: Vec<_> = infos.iter().map(FileInfo::encode).collect();
        if reply.len() + inline.iter().map(Vec::len).sum::<usize>() + inline.len() * 6
            < MAX_WIRE_BYTES - 256
        {
            for file in inline {
                wire::put_bytes(3, &file, &mut reply);
            }
        }
        let cancel = self.closed.child_token();
        {
            let mut tasks = self.downloads.lock();
            if tasks.len() >= MAX_TASKS || tasks.contains_key(&id.task) {
                return Err(PROTOCOL);
            }
            tasks.insert(id.task, cancel.clone());
        }
        if let Err(error) = self.send(request, true, 2, &reply).await {
            self.downloads.lock().remove(&id.task);
            return Err(error);
        }
        let connection = self.clone();
        tokio::spawn(async move {
            let result = connection
                .send_files(id.task, &access, &files, &cancel)
                .await;
            drop(access);
            let body = status(
                Id {
                    task: id.task,
                    file: u64::MAX,
                }
                .payload(),
                2,
                result.err().unwrap_or(OK),
            );
            let request = connection.request_id.fetch_add(1, Ordering::Relaxed);
            let _ = connection.send(request, false, 9, &body).await;
            connection.downloads.lock().remove(&id.task);
        });
        Ok(())
    }

    async fn send_files(
        &self,
        task: u64,
        access: &Access,
        files: &[(FileInfo, Location)],
        cancel: &CancellationToken,
    ) -> Result<(), u64> {
        for (index, (info, location)) in files.iter().enumerate() {
            if cancel.is_cancelled() {
                return Err(CANCELED);
            }
            let id = Id {
                task,
                file: index as u64 + 1,
            };
            let mut file = access.open_file(location, false).map_err(|e| error(&e))?;
            let meta = file.metadata().map_err(|e| error(&e))?;
            if meta.len() != info.size || modified(&meta) != info.modified {
                return Err(IO);
            }
            let mut ask = id.payload();
            wire::put_varint(2, info.modified, &mut ask);
            wire::put_varint(3, info.size, &mut ask);
            let reply = self.request(3, &ask, cancel).await?;
            let fields = check_reply(&reply, 3, id)?;
            if number(&fields, 2) != 0 {
                continue;
            }
            let mut position = number(&fields, 4);
            if position > info.size {
                return Err(PROTOCOL);
            }
            file.seek(SeekFrom::Start(position))
                .map_err(|e| error(&e))?;
            let mut block = position / BLOCK_BYTES as u64;
            let mut buffer = vec![0; BLOCK_BYTES];
            while position < info.size {
                let remaining = (info.size - position).min(BLOCK_BYTES as u64) as usize;
                file.read_exact(&mut buffer[..remaining])
                    .map_err(|e| error(&e))?;
                let mut body = id.payload();
                wire::put_varint(2, block, &mut body);
                wire::put_bytes(3, &buffer[..remaining], &mut body);
                let reply = self.request(4, &body, cancel).await?;
                let fields = check_reply(&reply, 4, id)?;
                if number(&fields, 2) != block || number(&fields, 4) != remaining as u64 {
                    return Err(PROTOCOL);
                }
                position += remaining as u64;
                block = block.checked_add(1).ok_or(PROTOCOL)?;
            }
            let reply = self
                .request(9, &status(id.payload(), 2, OK), cancel)
                .await?;
            check_reply(&reply, 6, id)?;
        }
        Ok(())
    }
}

fn check_reply(reply: &Reply, case: u32, id: Id) -> Result<Vec<wire::WireField<'_>>, u64> {
    if reply.case != case {
        return Err(PROTOCOL);
    }
    let fields = wire::decode(&reply.body).map_err(|_| PROTOCOL)?;
    if Id::parse(&fields)? != id {
        return Err(PROTOCOL);
    }
    let code = number(&fields, if case == 6 { 2 } else { 3 });
    if ![0, OK].contains(&code) {
        return Err(code);
    }
    Ok(fields)
}

fn modified(meta: &std::fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn duplicate(access: &Access, original: &Location) -> Result<(File, Location), u64> {
    let name = original.parts.last().ok_or(PERMISSION)?;
    let path = std::path::Path::new(name);
    let stem = path.file_stem().and_then(|s| s.to_str()).ok_or(PROTOCOL)?;
    let extension = path
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| format!(".{s}"))
        .unwrap_or_default();
    for index in 0..1024 {
        let mut candidate = original.clone();
        let name = if index == 0 {
            name.clone()
        } else {
            format!("{stem} ({index}){extension}")
        };
        if name.len() > 255 {
            return Err(PROTOCOL);
        }
        *candidate.parts.last_mut().unwrap() = name;
        match access.create_exclusive(&candidate) {
            Ok(file) => return Ok((file, candidate)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(error(&e)),
        }
    }
    Err(PROTOCOL)
}

fn collect(
    access: &Access,
    directory: &Location,
    prefix: &str,
    files: &mut Vec<(FileInfo, Location)>,
) -> Result<(), u64> {
    for entry in access.entries(directory).map_err(|e| error(&e))? {
        let location = directory.child(&entry.name).map_err(|e| error(&e))?;
        let name = if prefix.is_empty() {
            entry.name
        } else {
            format!("{prefix}/{}", entry.name)
        };
        if entry.directory {
            collect(access, &location, &name, files)?;
        } else {
            if files.len() >= MAX_FILES || entry.size > MAX_FILE_BYTES {
                return Err(PROTOCOL);
            }
            files.push((
                FileInfo {
                    path: name,
                    size: entry.size,
                    modified: entry.modified,
                },
                location,
            ));
        }
    }
    Ok(())
}

fn status(mut body: Vec<u8>, field: u32, code: u64) -> Vec<u8> {
    wire::put_varint(field, code, &mut body);
    body
}
fn entry_payload(kind: u64, name: &str, size: u64, modified: u64, path: &str) -> Vec<u8> {
    let mut entry = Vec::new();
    wire::put_varint(1, kind, &mut entry);
    wire::put_bytes(2, name.as_bytes(), &mut entry);
    wire::put_varint(3, size, &mut entry);
    wire::put_varint(4, modified, &mut entry);
    wire::put_bytes(5, path.as_bytes(), &mut entry);
    entry
}
