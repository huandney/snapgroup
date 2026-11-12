//! Espaço que um conjunto de subvolumes libera ao ser apagado, exato, lido dos
//! metadados do btrfs.
//!
//! Snapshot não copia nada: cada subvolume é uma raiz que aponta para blocos
//! de árvore compartilhados com os outros. O btrfs só mede o que um item tem
//! sozinho ("exclusivo"); o que dois itens marcados dividem apenas entre si
//! fica de fora, e é justamente o que sai ao apagar os dois. Aqui cada bloco de
//! metadados e cada extent de dados ganha o conjunto exato de subvolumes que o
//! alcança, percorrendo cada bloco compartilhado uma vez só, de cima para
//! baixo. Apagar um conjunto libera tudo cujo conjunto está contido nele.
//!
//! O kernel não expõe esse grafo sem quotas (qgroups), então os blocos são
//! lidos direto da partição, só leitura e com O_DIRECT. Suporta disco único,
//! crc32c e perfis single/DUP; fora disso, recusa.

use anyhow::{Result, bail};
use std::alloc::{Layout, alloc_zeroed, dealloc, handle_alloc_error};
use std::collections::HashMap;
use std::fmt;
use std::fs::{File, OpenOptions};
use std::os::unix::fs::{FileExt, OpenOptionsExt};
use std::path::Path;
use std::time::{Duration, Instant};

const SUPER_OFFSET: u64 = 0x10000;
const SUPER_SIZE: usize = 4096;
const MAGIC: u64 = 0x4D5F_5366_5248_425F; // "_BHRfS_M"
const SYS_CHUNK_ARRAY: usize = 0x32b;
const METADATA_UUID_FLAG: u64 = 1 << 10;

const HEADER: usize = 101;
const KEY_SIZE: usize = 17;
const LEAF_ITEM: usize = KEY_SIZE + 8;
const KEY_PTR: usize = KEY_SIZE + 16;

const EXTENT_DATA_KEY: u8 = 108;
const ROOT_ITEM_KEY: u8 = 132;
const CHUNK_ITEM_KEY: u8 = 228;

const FS_TREE: u64 = 5;
const FIRST_FREE: u64 = 256;
const LAST_FREE: u64 = -256i64 as u64;
const DATA_RELOC_TREE: u64 = -9i64 as u64;

const BLOCK_GROUP_METADATA: u64 = 1 << 2;
const BLOCK_GROUP_DUP: u64 = 1 << 5;
/// RAID0/1/10/5/6/1C3/1C4: mais de um disco ou stripes; fora do escopo.
const BLOCK_GROUP_RAID: u64 = (1 << 3) | (1 << 4) | (1 << 6) | (1 << 7) | (1 << 8) | (1 << 9) | (1 << 10);

const FILE_EXTENT_INLINE: u8 = 0;

/// Leituras em paralelo: o gargalo é a fila do NVMe, não a CPU. Medido num
/// disco com 460 mil blocos: 8 → 9.4 s, 16 → 7.1 s, 32 → 6.0 s, 64 → 5.9 s.
const THREADS: usize = 32;
const BATCH: usize = 32 * 1024;
const ATTEMPTS: usize = 3;

/// Bloco que não bate com o que o ponteiro prometia: o btrfs reaproveitou o
/// endereço depois de um commit durante a leitura. A varredura recomeça.
#[derive(Debug)]
struct Stale(u64);

impl fmt::Display for Stale {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "bloco {} mudou durante a leitura", self.0)
    }
}

impl std::error::Error for Stale {}

pub struct ScanStats {
    pub subvolumes: usize,
    pub dead_subvolumes: usize,
    pub blocks: usize,
    pub extents: usize,
    pub sets: usize,
    pub data_bytes: u64,
    pub metadata_bytes: u64,
    pub attempts: usize,
    pub elapsed: Duration,
    /// Parte do tempo gasta lendo e conferindo blocos (o resto é a conta).
    pub reading: Duration,
}

/// Uso do disco por conjunto de subvolumes. `freed` responde em microssegundos
/// para qualquer seleção: só percorre os conjuntos distintos.
pub struct SpaceMap {
    root_index: HashMap<u64, usize>,
    words: usize,
    /// (conjunto como bitset sobre `root_index`, bytes de dados, bytes de metadados)
    usage: Vec<(Box<[u64]>, u64, u64)>,
    pub stats: ScanStats,
}

#[derive(Clone, Copy, Default)]
pub struct Freed {
    pub data: u64,
    pub metadata: u64,
}

impl Freed {
    pub fn total(self) -> u64 {
        self.data + self.metadata
    }
}

impl SpaceMap {
    /// Bytes que saem do disco se todos os `tree_ids` forem apagados. Ids
    /// desconhecidos são ignorados.
    pub fn freed(&self, tree_ids: &[u64]) -> Freed {
        let mut mask = vec![0u64; self.words];
        for id in tree_ids {
            if let Some(&i) = self.root_index.get(id) {
                mask[i / 64] |= 1 << (i % 64);
            }
        }
        let mut freed = Freed::default();
        for (set, data, metadata) in &self.usage {
            if set.iter().zip(&mask).all(|(s, m)| s & !m == 0) {
                freed.data += data;
                freed.metadata += metadata;
            }
        }
        freed
    }
}

/// Lê os metadados de `device` (a partição do btrfs montado) e monta o mapa.
pub fn scan(device: &Path) -> Result<SpaceMap> {
    let start = Instant::now();
    let mut attempt = 1;
    loop {
        match scan_once(device) {
            Ok(mut map) => {
                map.stats.attempts = attempt;
                map.stats.elapsed = start.elapsed();
                return Ok(map);
            }
            Err(e) if e.downcast_ref::<Stale>().is_some() && attempt < ATTEMPTS => attempt += 1,
            Err(e) => return Err(e),
        }
    }
}

fn scan_once(device: &Path) -> Result<SpaceMap> {
    let disk = Disk::open(device)?;
    let roots = disk.fs_roots()?;
    let live: Vec<&Root> = roots.iter().filter(|r| r.refs > 0).collect();
    let dead_subvolumes = roots.len() - live.len();

    let words = live.len().div_ceil(64).max(1);
    let mut sets = Interner::new(words);
    let mut root_index = HashMap::new();
    let mut by_level: Vec<HashMap<u64, Pending>> = (0..8).map(|_| HashMap::new()).collect();
    for (i, root) in live.iter().enumerate() {
        root_index.insert(root.id, i);
        let single = sets.single(i);
        let level = usize::from(root.level).min(7);
        merge_pending(&mut sets, &mut by_level[level], root.bytenr, single, None);
    }

    let meta_mult = disk.metadata_copies();
    let mut meta_by_set: HashMap<u32, u64> = HashMap::new();
    let mut extents: HashMap<u64, (u32, u64)> = HashMap::new();
    let mut blocks = 0;
    let mut reading = Duration::ZERO;
    for level in (0..by_level.len()).rev() {
        let mut pending: Vec<(u64, Pending)> = by_level[level].drain().collect();
        pending.sort_unstable_by_key(|(bytenr, _)| *bytenr);
        blocks += pending.len();
        for batch in pending.chunks(BATCH) {
            for (_, p) in batch {
                *meta_by_set.entry(p.set).or_default() += disk.nodesize as u64 * meta_mult;
            }
            let started = Instant::now();
            let found = read_batch(&disk, batch, level as u8)?;
            reading += started.elapsed();
            for item in found {
                match item {
                    Found::Child { bytenr, generation, set } => {
                        merge_pending(&mut sets, &mut by_level[level - 1], bytenr, set, Some(generation));
                    }
                    Found::Extent { bytenr, bytes, set } => {
                        let entry = extents.entry(bytenr).or_insert((set, bytes));
                        entry.0 = sets.union(entry.0, set);
                    }
                }
            }
        }
    }

    let mut data_by_set: HashMap<u32, u64> = HashMap::new();
    let mut data_bytes = 0;
    for (set, bytes) in extents.values() {
        *data_by_set.entry(*set).or_default() += bytes;
        data_bytes += bytes;
    }
    let metadata_bytes = meta_by_set.values().sum();
    let mut ids: Vec<u32> = data_by_set.keys().chain(meta_by_set.keys()).copied().collect();
    ids.sort_unstable();
    ids.dedup();
    let usage = ids
        .iter()
        .map(|&id| {
            (
                sets.get(id).into(),
                data_by_set.get(&id).copied().unwrap_or(0),
                meta_by_set.get(&id).copied().unwrap_or(0),
            )
        })
        .collect();

    Ok(SpaceMap {
        root_index,
        words,
        usage,
        stats: ScanStats {
            subvolumes: live.len(),
            dead_subvolumes,
            blocks,
            extents: extents.len(),
            sets: ids.len(),
            data_bytes,
            metadata_bytes,
            attempts: 0,
            elapsed: Duration::ZERO,
            reading,
        },
    })
}

#[derive(Clone, Copy)]
struct Pending {
    set: u32,
    /// Geração prometida pelo ponteiro do pai; raízes não têm.
    generation: Option<u64>,
}

fn merge_pending(
    sets: &mut Interner,
    level: &mut HashMap<u64, Pending>,
    bytenr: u64,
    set: u32,
    generation: Option<u64>,
) {
    match level.get_mut(&bytenr) {
        Some(p) => p.set = sets.union(p.set, set),
        None => {
            level.insert(bytenr, Pending { set, generation });
        }
    }
}

enum Found {
    Child { bytenr: u64, generation: u64, set: u32 },
    Extent { bytenr: u64, bytes: u64, set: u32 },
}

/// Lê um lote de blocos do mesmo nível em paralelo e devolve filhos (nós) ou
/// extents de dados (folhas), já com o conjunto do bloco que os aponta.
fn read_batch(disk: &Disk, batch: &[(u64, Pending)], level: u8) -> Result<Vec<Found>> {
    let per_thread = batch.len().div_ceil(THREADS).max(1);
    let results: Vec<Result<Vec<Found>>> = std::thread::scope(|scope| {
        let handles: Vec<_> = batch
            .chunks(per_thread)
            .map(|part| scope.spawn(move || read_part(disk, part, level)))
            .collect();
        handles.into_iter().map(|h| h.join().expect("thread de leitura")).collect()
    });
    let mut found = Vec::new();
    for result in results {
        found.extend(result?);
    }
    Ok(found)
}

fn read_part(disk: &Disk, part: &[(u64, Pending)], level: u8) -> Result<Vec<Found>> {
    let mut buf = Aligned::new(disk.nodesize);
    let mut found = Vec::new();
    for &(bytenr, pending) in part {
        disk.read_tree_block(bytenr, &mut buf, pending.generation, Some(level))?;
        if level > 0 {
            for (_, child, generation) in node_ptrs(&buf, bytenr)? {
                found.push(Found::Child { bytenr: child, generation, set: pending.set });
            }
            continue;
        }
        for (key, data) in leaf_items(&buf, bytenr)? {
            if key.kind != EXTENT_DATA_KEY {
                continue;
            }
            if let Some((disk_bytenr, bytes)) = file_extent_disk_range(data) {
                found.push(Found::Extent { bytenr: disk_bytenr, bytes, set: pending.set });
            }
        }
    }
    Ok(found)
}

/// Extent em disco referenciado por um `btrfs_file_extent_item`. Inline vive
/// na folha (já contada como metadado); `disk_bytenr == 0` é buraco.
fn file_extent_disk_range(data: &[u8]) -> Option<(u64, u64)> {
    if data.len() < 53 || data[20] == FILE_EXTENT_INLINE {
        return None;
    }
    let disk_bytenr = le64(data, 21);
    if disk_bytenr == 0 {
        return None;
    }
    Some((disk_bytenr, le64(data, 29)))
}

/// Conjuntos de raízes como bitsets, internados: milhões de extents caem em
/// poucos milhares de conjuntos distintos, e cada união é calculada uma vez.
struct Interner {
    words: usize,
    sets: Vec<Box<[u64]>>,
    index: HashMap<Box<[u64]>, u32>,
    unions: HashMap<(u32, u32), u32>,
}

impl Interner {
    fn new(words: usize) -> Self {
        Self { words, sets: Vec::new(), index: HashMap::new(), unions: HashMap::new() }
    }

    fn intern(&mut self, set: Box<[u64]>) -> u32 {
        if let Some(&id) = self.index.get(&set) {
            return id;
        }
        let id = self.sets.len() as u32;
        self.sets.push(set.clone());
        self.index.insert(set, id);
        id
    }

    fn single(&mut self, root: usize) -> u32 {
        let mut set = vec![0u64; self.words].into_boxed_slice();
        set[root / 64] |= 1 << (root % 64);
        self.intern(set)
    }

    fn union(&mut self, a: u32, b: u32) -> u32 {
        if a == b {
            return a;
        }
        let key = (a.min(b), a.max(b));
        if let Some(&id) = self.unions.get(&key) {
            return id;
        }
        let set: Box<[u64]> = self.sets[a as usize]
            .iter()
            .zip(self.sets[b as usize].iter())
            .map(|(x, y)| x | y)
            .collect();
        let id = self.intern(set);
        self.unions.insert(key, id);
        id
    }

    fn get(&self, id: u32) -> &[u64] {
        &self.sets[id as usize]
    }
}

struct Root {
    id: u64,
    bytenr: u64,
    level: u8,
    refs: u32,
}

#[derive(Clone, Copy)]
struct Chunk {
    logical: u64,
    length: u64,
    physical: u64,
    flags: u64,
}

struct Disk {
    file: File,
    nodesize: usize,
    fsid: [u8; 16],
    chunks: Vec<Chunk>,
    root_tree: (u64, u8),
}

impl Disk {
    fn open(device: &Path) -> Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECT)
            .open(device)
            .map_err(|e| anyhow::anyhow!("abrir {}: {e}", device.display()))?;
        let mut sb = Aligned::new(SUPER_SIZE);
        file.read_exact_at(&mut sb, SUPER_OFFSET)
            .map_err(|e| anyhow::anyhow!("ler superbloco: {e}"))?;
        if le64(&sb, 0x40) != MAGIC {
            bail!("{} não é um btrfs", device.display());
        }
        if le16(&sb, 0xc4) != 0 {
            bail!("checksum do btrfs não é crc32c; estimativa exata indisponível");
        }
        if crc32c(&sb[32..]) != le32(&sb, 0) {
            bail!("checksum do superbloco não confere");
        }
        if le64(&sb, 0x88) != 1 {
            bail!("btrfs com mais de um disco; estimativa exata indisponível");
        }
        let nodesize = le32(&sb, 0x94) as usize;
        let incompat = le64(&sb, 0xbc);
        let uuid_at = if incompat & METADATA_UUID_FLAG != 0 { 0x23b } else { 0x20 };
        let mut fsid = [0u8; 16];
        fsid.copy_from_slice(&sb[uuid_at..uuid_at + 16]);

        let mut disk = Disk {
            file,
            nodesize,
            fsid,
            chunks: Vec::new(),
            root_tree: (le64(&sb, 0x50), sb[0xc6]),
        };
        let array_size = le32(&sb, 0xa0) as usize;
        disk.chunks = parse_sys_chunk_array(&sb[SYS_CHUNK_ARRAY..SYS_CHUNK_ARRAY + array_size])?;
        disk.sort_chunks();

        let mut chunks = Vec::new();
        disk.walk_leaves(le64(&sb, 0x58), sb[0xc7], &mut |key, data| {
            if key.kind == CHUNK_ITEM_KEY {
                chunks.push(parse_chunk(key.offset, data)?);
            }
            Ok(())
        })?;
        disk.chunks = chunks;
        disk.sort_chunks();
        Ok(disk)
    }

    fn sort_chunks(&mut self) {
        self.chunks.sort_unstable_by_key(|c| c.logical);
    }

    /// Cópias de cada bloco de metadados no disco: DUP grava duas.
    fn metadata_copies(&self) -> u64 {
        let dup = self
            .chunks
            .iter()
            .any(|c| c.flags & BLOCK_GROUP_METADATA != 0 && c.flags & BLOCK_GROUP_DUP != 0);
        if dup { 2 } else { 1 }
    }

    fn physical(&self, logical: u64) -> Result<u64> {
        let i = self.chunks.partition_point(|c| c.logical <= logical);
        let Some(chunk) = i.checked_sub(1).map(|i| self.chunks[i]) else {
            bail!(Stale(logical));
        };
        if logical >= chunk.logical + chunk.length {
            bail!(Stale(logical));
        }
        Ok(chunk.physical + (logical - chunk.logical))
    }

    fn read_tree_block(
        &self,
        logical: u64,
        buf: &mut Aligned,
        generation: Option<u64>,
        level: Option<u8>,
    ) -> Result<()> {
        let physical = self.physical(logical)?;
        self.file
            .read_exact_at(buf, physical)
            .map_err(|e| anyhow::anyhow!("ler bloco {logical}: {e}"))?;
        let stale = crc32c(&buf[32..]) != le32(buf, 0)
            || le64(buf, 0x30) != logical
            || buf[0x20..0x30] != self.fsid
            || generation.is_some_and(|g| le64(buf, 0x50) != g)
            || level.is_some_and(|l| buf[0x64] != l);
        if stale {
            bail!(Stale(logical));
        }
        Ok(())
    }

    /// Percorre uma árvore pequena (chunk, root) chamando `f` em cada item.
    fn walk_leaves(
        &self,
        bytenr: u64,
        level: u8,
        f: &mut dyn FnMut(Key, &[u8]) -> Result<()>,
    ) -> Result<()> {
        let mut buf = Aligned::new(self.nodesize);
        self.read_tree_block(bytenr, &mut buf, None, Some(level))?;
        if level == 0 {
            for (key, data) in leaf_items(&buf, bytenr)? {
                f(key, data)?;
            }
            return Ok(());
        }
        let children = node_ptrs(&buf, bytenr)?;
        for (_, child, _) in children {
            self.walk_leaves(child, level - 1, f)?;
        }
        Ok(())
    }

    /// Raízes que podem segurar dados de arquivo: a FS_TREE, os subvolumes e a
    /// árvore de relocação. `refs == 0` é subvolume já apagado que o cleaner
    /// do btrfs ainda está desmontando.
    fn fs_roots(&self) -> Result<Vec<Root>> {
        let mut roots = Vec::new();
        let (bytenr, level) = self.root_tree;
        self.walk_leaves(bytenr, level, &mut |key, data| {
            let is_fs = key.objectid == FS_TREE
                || (FIRST_FREE..=LAST_FREE).contains(&key.objectid)
                || key.objectid == DATA_RELOC_TREE;
            if key.kind != ROOT_ITEM_KEY || !is_fs || data.len() < 239 {
                return Ok(());
            }
            roots.push(Root {
                id: key.objectid,
                bytenr: le64(data, 176),
                level: data[238],
                refs: le32(data, 216),
            });
            Ok(())
        })?;
        Ok(roots)
    }
}

#[derive(Clone, Copy)]
struct Key {
    objectid: u64,
    kind: u8,
    offset: u64,
}

fn parse_key(buf: &[u8], at: usize) -> Key {
    Key { objectid: le64(buf, at), kind: buf[at + 8], offset: le64(buf, at + 9) }
}

fn leaf_items(buf: &[u8], bytenr: u64) -> Result<Vec<(Key, &[u8])>> {
    let count = le32(buf, 0x60) as usize;
    let mut items = Vec::with_capacity(count);
    for i in 0..count {
        let at = HEADER + i * LEAF_ITEM;
        if at + LEAF_ITEM > buf.len() {
            bail!(Stale(bytenr));
        }
        let start = HEADER + le32(buf, at + KEY_SIZE) as usize;
        let end = start + le32(buf, at + KEY_SIZE + 4) as usize;
        if end > buf.len() {
            bail!(Stale(bytenr));
        }
        items.push((parse_key(buf, at), &buf[start..end]));
    }
    Ok(items)
}

fn node_ptrs(buf: &[u8], bytenr: u64) -> Result<Vec<(Key, u64, u64)>> {
    let count = le32(buf, 0x60) as usize;
    let mut ptrs = Vec::with_capacity(count);
    for i in 0..count {
        let at = HEADER + i * KEY_PTR;
        if at + KEY_PTR > buf.len() {
            bail!(Stale(bytenr));
        }
        ptrs.push((parse_key(buf, at), le64(buf, at + KEY_SIZE), le64(buf, at + KEY_SIZE + 8)));
    }
    Ok(ptrs)
}

fn parse_sys_chunk_array(array: &[u8]) -> Result<Vec<Chunk>> {
    let mut chunks = Vec::new();
    let mut at = 0;
    while at + KEY_SIZE <= array.len() {
        let key = parse_key(array, at);
        at += KEY_SIZE;
        if key.kind != CHUNK_ITEM_KEY || at + 48 > array.len() {
            bail!("sys_chunk_array inesperado");
        }
        let stripes = usize::from(le16(array, at + 44));
        let size = 48 + 32 * stripes;
        if at + size > array.len() {
            bail!("sys_chunk_array truncado");
        }
        chunks.push(parse_chunk(key.offset, &array[at..at + size])?);
        at += size;
    }
    Ok(chunks)
}

/// `btrfs_chunk`: single e DUP mapeiam o lógico para a 1ª stripe, no único disco.
fn parse_chunk(logical: u64, data: &[u8]) -> Result<Chunk> {
    if data.len() < 48 + 32 {
        bail!("chunk {logical} truncado");
    }
    let flags = le64(data, 24);
    if flags & BLOCK_GROUP_RAID != 0 {
        bail!("perfil RAID no chunk {logical}; estimativa exata indisponível");
    }
    Ok(Chunk { logical, length: le64(data, 0), physical: le64(data, 48 + 8), flags })
}

fn le16(buf: &[u8], at: usize) -> u16 {
    u16::from_le_bytes(buf[at..at + 2].try_into().expect("2 bytes"))
}

fn le32(buf: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(buf[at..at + 4].try_into().expect("4 bytes"))
}

fn le64(buf: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(buf[at..at + 8].try_into().expect("8 bytes"))
}

/// Buffer alinhado a 4 KiB, exigido pelo O_DIRECT.
struct Aligned {
    ptr: *mut u8,
    len: usize,
}

impl Aligned {
    fn new(len: usize) -> Self {
        let layout = Layout::from_size_align(len, 4096).expect("layout alinhado");
        // SAFETY: layout tem tamanho > 0 (nodesize/superbloco).
        let ptr = unsafe { alloc_zeroed(layout) };
        if ptr.is_null() {
            handle_alloc_error(layout);
        }
        Self { ptr, len }
    }
}

impl std::ops::Deref for Aligned {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        // SAFETY: ptr aponta para `len` bytes alocados e inicializados por nós.
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }
}

impl std::ops::DerefMut for Aligned {
    fn deref_mut(&mut self) -> &mut [u8] {
        // SAFETY: idem, com acesso exclusivo via &mut self.
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }
    }
}

impl Drop for Aligned {
    fn drop(&mut self) {
        let layout = Layout::from_size_align(self.len, 4096).expect("layout alinhado");
        // SAFETY: mesmo ptr e layout da alocação.
        unsafe { dealloc(self.ptr, layout) };
    }
}

fn crc32c(data: &[u8]) -> u32 {
    #[cfg(target_arch = "x86_64")]
    if std::arch::is_x86_feature_detected!("sse4.2") {
        // SAFETY: a instrução crc32 existe, detectado acima.
        return unsafe { crc32c_sse42(data) };
    }
    crc32c_table(data)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.2")]
unsafe fn crc32c_sse42(data: &[u8]) -> u32 {
    use std::arch::x86_64::{_mm_crc32_u8, _mm_crc32_u64};
    let mut crc = u64::from(u32::MAX);
    let mut words = data.chunks_exact(8);
    for word in &mut words {
        crc = _mm_crc32_u64(crc, u64::from_le_bytes(word.try_into().expect("8 bytes")));
    }
    let mut crc = crc as u32;
    for &byte in words.remainder() {
        crc = _mm_crc32_u8(crc, byte);
    }
    !crc
}

const CRC32C_TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut crc = i as u32;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0x82F6_3B78 } else { crc >> 1 };
            bit += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
};

fn crc32c_table(data: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for &byte in data {
        crc = CRC32C_TABLE[((crc ^ u32::from(byte)) & 0xff) as usize] ^ (crc >> 8);
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::{Interner, crc32c, crc32c_table, file_extent_disk_range};

    #[test]
    fn crc32c_matches_the_standard_check_value() {
        assert_eq!(crc32c_table(b"123456789"), 0xE306_9283);
        assert_eq!(crc32c(b"123456789"), 0xE306_9283);
        let long: Vec<u8> = (0..16_381u32).map(|i| (i * 7 + 3) as u8).collect();
        assert_eq!(crc32c(&long), crc32c_table(&long));
    }

    #[test]
    fn interner_reuses_sets_and_unions() {
        let mut sets = Interner::new(1);
        let a = sets.single(0);
        let b = sets.single(1);
        let ab = sets.union(a, b);
        assert_eq!(sets.union(b, a), ab);
        assert_eq!(sets.union(ab, a), ab);
        assert_eq!(sets.get(ab), &[0b11]);
        assert_eq!(sets.single(0), a);
    }

    /// Prevê o espaço liberado, apaga e confere com o que o btrfs informa.
    /// Rodar como root, com um btrfs vazio montado pelo top-level:
    ///   cargo test --no-run && sudo SNAPG_TEST_BTRFS=/mnt/x <binário> --ignored
    #[test]
    #[ignore = "precisa de root e de um btrfs vazio montado em SNAPG_TEST_BTRFS"]
    fn predicted_space_matches_real_deletion_on_real_btrfs() {
        use std::path::{Path, PathBuf};
        use std::process::{Command, Stdio};
        let top = PathBuf::from(std::env::var("SNAPG_TEST_BTRFS").expect("SNAPG_TEST_BTRFS"));
        let run = |args: &[&str]| {
            let status = Command::new(args[0])
                .args(&args[1..])
                .current_dir(&top)
                .stdout(Stdio::null())
                .status()
                .unwrap();
            assert!(status.success(), "{args:?}");
        };
        let mut seed = 0x9E37_79B9_7F4A_7C15u64;
        let mut write = |path: &str, mib: usize| {
            let mut data = vec![0u8; mib << 20];
            for chunk in data.chunks_mut(8) {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                chunk.copy_from_slice(&seed.to_le_bytes()[..chunk.len()]);
            }
            std::fs::write(top.join(path), data).unwrap();
        };
        let data_used = || {
            run(&["btrfs", "filesystem", "sync", "."]);
            let out = Command::new("btrfs")
                .args(["filesystem", "usage", "-b"])
                .arg(&top)
                .output()
                .unwrap();
            let text = String::from_utf8_lossy(&out.stdout).to_string();
            let line = text.lines().find(|l| l.starts_with("Data,")).expect("linha Data");
            let used = line.split("Used:").nth(1).unwrap().split_whitespace().next().unwrap();
            used.trim_end_matches(',').parse::<u64>().unwrap()
        };
        let id = |name: &str| crate::btrfs::subvol_id(&top.join(name)).unwrap();

        run(&["btrfs", "subvolume", "create", "sp_live"]);
        write("sp_live/a", 8);
        write("sp_live/b", 4);
        write("sp_live/c", 2);
        run(&["btrfs", "subvolume", "snapshot", "-r", "sp_live", "sp_s1"]);
        write("sp_live/a", 8); // reescrito: o "a" antigo fica só em s1
        write("sp_live/d", 3);
        run(&["btrfs", "subvolume", "snapshot", "-r", "sp_live", "sp_s2"]);
        std::fs::remove_file(top.join("sp_live/b")).unwrap(); // "b" fica em s1 e s2
        let (s1, s2) = (id("sp_s1"), id("sp_s2"));

        let before = data_used();
        let device = crate::btrfs::fs_device(top.to_str().unwrap()).unwrap();
        let map = super::scan(Path::new(&device)).unwrap();
        let only_s1 = map.freed(&[s1]);
        let both = map.freed(&[s1, s2]);
        assert_eq!(only_s1.data, 8 << 20, "s1 sozinho libera só o \"a\" antigo");
        assert_eq!(both.data, (8 << 20) + (4 << 20), "s1 + s2 liberam também o \"b\"");

        run(&["btrfs", "subvolume", "delete", "sp_s1"]);
        run(&["btrfs", "subvolume", "sync", "."]);
        let after_s1 = data_used();
        assert_eq!(before - after_s1, only_s1.data);
        run(&["btrfs", "subvolume", "delete", "sp_s2"]);
        run(&["btrfs", "subvolume", "sync", "."]);
        assert_eq!(before - data_used(), both.data);

        run(&["btrfs", "subvolume", "delete", "sp_live"]);
    }

    #[test]
    fn file_extents_skip_inline_and_holes() {
        let mut item = vec![0u8; 53];
        item[20] = 1; // regular
        item[21..29].copy_from_slice(&4096u64.to_le_bytes());
        item[29..37].copy_from_slice(&8192u64.to_le_bytes());
        assert_eq!(file_extent_disk_range(&item), Some((4096, 8192)));
        item[21..29].copy_from_slice(&0u64.to_le_bytes());
        assert_eq!(file_extent_disk_range(&item), None);
        item[20] = 0; // inline
        assert_eq!(file_extent_disk_range(&item[..30]), None);
    }
}
