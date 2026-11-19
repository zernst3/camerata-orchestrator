//! Cutting a unit into pieces (design section 1). A piece is the files one agent reviews. The
//! cut is by folder, in path order, and is the same on every run.

use std::collections::BTreeMap;

use sha2::{Digest, Sha256};

use super::Piece;

/// The size a piece aims for: 240,000 bytes of model-facing files, about 6,000 lines.
pub const TARGET_BYTES: usize = 240_000;

/// A unit at or under this many hundredths of the target is one piece (125 means 1.25 times).
const ONE_PIECE_PERCENT: usize = 125;

/// A unit of the codebase: pieces never cross it. Until service detection lands there is one unit
/// for each repository ([`units_for`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unit {
    pub name: String,
    /// Repo-relative paths of the unit's files.
    pub files: Vec<String>,
}

/// The units of a repository. One unit for the whole repository: the one place service
/// detection plugs in later. A repository with no files has no units.
pub fn units_for(repo: &str, files: &[(String, String)]) -> Vec<Unit> {
    if files.is_empty() {
        return Vec::new();
    }
    let mut paths: Vec<String> = files.iter().map(|(p, _)| p.clone()).collect();
    paths.sort();
    paths.dedup();
    vec![Unit {
        name: repo.to_string(),
        files: paths,
    }]
}

/// A run of neighbouring files that is kept together while it fits the target.
struct Block {
    /// Indexes into the unit's sorted file list.
    files: Vec<usize>,
    bytes: usize,
}

/// Cut `units` into pieces. `files` holds the model-facing text (the size is its byte length).
/// `target_bytes` is the piece size to aim for. `force_pieces = Some(k)` (internal runs) sets the
/// target of each unit to its size divided by `k` and guarantees at least `k` pieces when the
/// unit has that many files.
pub fn plan_pieces(
    units: &[Unit],
    files: &[(String, String)],
    target_bytes: usize,
    force_pieces: Option<usize>,
) -> Vec<Piece> {
    let sizes: BTreeMap<&str, usize> = files.iter().map(|(p, c)| (p.as_str(), c.len())).collect();
    let mut out = Vec::new();
    for unit in units {
        let mut paths: Vec<&str> = unit.files.iter().map(String::as_str).collect();
        paths.sort_unstable();
        paths.dedup();
        if paths.is_empty() {
            continue;
        }
        let bytes: Vec<usize> = paths
            .iter()
            .map(|p| sizes.get(p).copied().unwrap_or(0))
            .collect();
        let total: usize = bytes.iter().sum();
        let groups: Vec<Vec<usize>> = match force_pieces {
            Some(k) if k >= 2 => forced_groups(&paths, &bytes, (total / k).max(1), k),
            Some(_) => vec![(0..paths.len()).collect()],
            None if total * 100 <= target_bytes.max(1) * ONE_PIECE_PERCENT => {
                vec![(0..paths.len()).collect()]
            }
            None => folder_groups(&paths, &bytes, target_bytes.max(1)),
        };
        for (n, group) in groups.iter().enumerate() {
            let group_paths: Vec<String> = group.iter().map(|&i| paths[i].to_string()).collect();
            out.push(Piece {
                key: piece_key(&unit.name, n + 1, &group_paths),
                unit: unit.name.clone(),
                files: group_paths,
            });
        }
    }
    out
}

/// The piece key: unit name, the piece number and a digest of the file list.
fn piece_key(unit: &str, number: usize, files: &[String]) -> String {
    let mut hasher = Sha256::new();
    for f in files {
        hasher.update(f.as_bytes());
        hasher.update(b"\n");
    }
    let digest = hasher.finalize();
    let hex: String = digest.iter().take(4).map(|b| format!("{b:02x}")).collect();
    format!("{unit}#{number}:{hex}")
}

/// The folder cut: blocks by folder, then neighbouring blocks packed up to the target.
fn folder_groups(paths: &[&str], bytes: &[usize], target: usize) -> Vec<Vec<usize>> {
    let all: Vec<usize> = (0..paths.len()).collect();
    let mut blocks = Vec::new();
    split_blocks(paths, bytes, &all, 0, target, &mut blocks);
    merge_small_tail(pack(blocks, target), bytes, target)
}

/// A last piece of one or two files joins the piece before it when the two stay within the
/// one piece allowance (1.25 times the target, the same allowance a whole unit gets to stay one
/// piece). The greedy pack leaves such a tail when the last block missed the previous piece by a
/// little, and a tiny piece costs a whole agent run.
fn merge_small_tail(
    mut groups: Vec<Vec<usize>>,
    bytes: &[usize],
    target: usize,
) -> Vec<Vec<usize>> {
    let group_bytes = |g: &[usize]| g.iter().map(|&m| bytes[m]).sum::<usize>();
    if groups.len() >= 2 {
        let tail = groups.len() - 1;
        let combined = group_bytes(&groups[tail]) + group_bytes(&groups[tail - 1]);
        if groups[tail].len() <= 2 && combined * 100 <= target * ONE_PIECE_PERCENT {
            let last = groups.pop().unwrap_or_default();
            groups[tail - 1].extend(last);
        }
    }
    groups
}

/// Turn `members` (indexes sharing the first `depth` path components) into blocks. A group that
/// fits the target is one block; a folder over the target is opened; a file over the target is a
/// block of its own.
fn split_blocks(
    paths: &[&str],
    bytes: &[usize],
    members: &[usize],
    depth: usize,
    target: usize,
    out: &mut Vec<Block>,
) {
    let mut i = 0;
    while i < members.len() {
        let key = child_key(paths[members[i]], depth);
        let mut j = i + 1;
        while j < members.len() && child_key(paths[members[j]], depth) == key {
            j += 1;
        }
        let group = &members[i..j];
        let size: usize = group.iter().map(|&m| bytes[m]).sum();
        let is_folder = has_more_components(paths[group[0]], depth + 1);
        if size <= target || !is_folder {
            out.push(Block {
                files: group.to_vec(),
                bytes: size,
            });
        } else {
            split_blocks(paths, bytes, group, depth + 1, target, out);
        }
        i = j;
    }
}

/// The path component at `depth` (0-based), with a marker saying whether it is a folder name, so
/// a file and a folder that share a name stay apart.
fn child_key(path: &str, depth: usize) -> (String, bool) {
    let comps: Vec<&str> = path.split('/').collect();
    let name = comps.get(depth).copied().unwrap_or("").to_string();
    (name, comps.len() > depth + 1)
}

/// Whether `path` has a component at `depth` or deeper (so the group at `depth - 1` is a folder).
fn has_more_components(path: &str, depth: usize) -> bool {
    path.split('/').count() > depth
}

/// Pack neighbouring blocks into pieces of at most `target` bytes. A block over the target stays
/// alone.
fn pack(blocks: Vec<Block>, target: usize) -> Vec<Vec<usize>> {
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut current: Vec<usize> = Vec::new();
    let mut current_bytes = 0usize;
    for block in blocks {
        if !current.is_empty() && current_bytes + block.bytes > target {
            groups.push(std::mem::take(&mut current));
            current_bytes = 0;
        }
        current_bytes += block.bytes;
        current.extend(block.files);
    }
    if !current.is_empty() {
        groups.push(current);
    }
    groups
}

/// The forced cut: the folder cut at the smaller target; when that gives more than `k` pieces,
/// the two neighbours with the fewest bytes together are joined until there are `k`; when it
/// gives fewer, the largest multi-file piece is halved (at its byte midpoint, in path order)
/// until there are `k` pieces or every piece is one file.
fn forced_groups(paths: &[&str], bytes: &[usize], target: usize, k: usize) -> Vec<Vec<usize>> {
    let mut groups = folder_groups(paths, bytes, target);
    while groups.len() > k.max(1) {
        let size = |g: &Vec<usize>| g.iter().map(|&m| bytes[m]).sum::<usize>();
        let at = (0..groups.len() - 1)
            .min_by_key(|&i| (size(&groups[i]) + size(&groups[i + 1]), i))
            .unwrap_or(0);
        let next = groups.remove(at + 1);
        groups[at].extend(next);
    }
    while groups.len() < k {
        let pick = groups
            .iter()
            .enumerate()
            .filter(|(_, g)| g.len() > 1)
            .max_by_key(|(i, g)| (g.iter().map(|&m| bytes[m]).sum::<usize>(), usize::MAX - *i))
            .map(|(i, _)| i);
        let Some(at) = pick else { break };
        let group = groups.remove(at);
        let total: usize = group.iter().map(|&m| bytes[m]).sum();
        let mut running = 0usize;
        let mut cut = 1usize;
        for (n, &m) in group.iter().enumerate() {
            running += bytes[m];
            if running * 2 >= total {
                cut = (n + 1).clamp(1, group.len() - 1);
                break;
            }
        }
        let (left, right) = group.split_at(cut);
        groups.insert(at, right.to_vec());
        groups.insert(at, left.to_vec());
    }
    groups
}
