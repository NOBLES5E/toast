use crate::{cache, format::CodeStr, spinner::spin};
use std::{
  fs,
  fs::{File, Metadata},
  io::{Seek, SeekFrom, Write},
  os::unix::fs::PermissionsExt,
  path::{Path, PathBuf},
  sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
  },
};
use tar::{Builder, Header};
use walkdir::WalkDir;

// Add a file to a tar archive.
fn add_file<W: Write>(
  builder: &mut Builder<W>,
  metadata: &Metadata,
  path: &Path,
  source_dir: &Path,
  destination_dir: &Path,
  file_hashes: &mut Vec<String>,
) -> Result<(), String> {
  // Compute the source and destination paths.
  let source = source_dir.join(&path);
  let mut destination = destination_dir.join(&path);

  // Tar archives must contain only relative paths. But for our purposes, the
  // paths will be relative to the filesystem root. [tag:destination_absolute]
  if destination.starts_with("/") {
    // The `unwrap` is safe due to [ref:destination_absolute]
    destination = destination.strip_prefix("/").unwrap().to_owned();
  }

  // Determine if the file has the executable bit set.
  let mode = metadata.permissions().mode();
  let executable = mode & 0o1 > 0 || mode & 0o10 > 0 || mode & 0o100 > 0;

  // Construct a tar header for this file.
  let mut header = Header::new_gnu();
  header.set_mode(if executable { 0o777 } else { 0o666 });
  header.set_size(metadata.len());

  // Open the file so we can compute the hash of its contents.
  let mut file = File::open(&source).map_err(|e| {
    format!(
      "Unable to open file {}. Details: {}",
      &source.to_string_lossy().code_str(),
      e
    )
  })?;

  // Compute the hash of the file contents and metadata.
  file_hashes.push(cache::extend(
    &cache::extend(
      &cache::hash_str(&path.to_string_lossy()),
      &cache::hash_read(&mut file)?,
    ),
    if executable { "+x" } else { "-x" },
  ));

  // Jump back to the beginning of the file so the tar builder can read it.
  file.seek(SeekFrom::Start(0)).map_err(|e| {
    format!(
      "Unable to seek file {}. Details: {}",
      &source.to_string_lossy().code_str(),
      e
    )
  })?;

  // Add the file to the archive and return.
  builder.append_data(&mut header, destination, file).unwrap();
  Ok(())
}

// Construct a tar archive and return a hash of its contents.
pub fn create<W: Write>(
  spinner_message: &str,
  writer: W,
  paths: &[PathBuf],
  source_dir: &Path,
  destination_dir: &Path,
  interrupted: &Arc<AtomicBool>,
) -> Result<(W, String), String> {
  // Render a spinner animation in the terminal.
  let _guard = spin(spinner_message);

  // Canonicalize the source directory such that other paths can be relativized
  // with respect to it.
  let source_dir = source_dir.canonicalize().map_err(|e| {
    format!(
      "Unable to canonicalize path {}. Details: {}",
      source_dir.to_string_lossy().code_str(),
      e
    )
  })?;

  // This vector will store all the hashes of the contents and metadata of all
  // the files in the archive. In the end, we will sort this vector and then
  // take the hash of the whole thing.
  let mut file_hashes = vec![];

  // This builder will be responsible for writing to the tar file.
  let mut builder = Builder::new(writer);
  builder.follow_symlinks(false);

  // Add each path to the archive.
  for path in paths {
    // If the user wants to stop the operation, quit now.
    if interrupted.load(Ordering::SeqCst) {
      return Err(super::INTERRUPT_MESSAGE.to_owned());
    }

    // Compute the source path.
    let source_path = source_dir.join(path);

    // Fetch the filesystem metadata for this path.
    let metadata = fs::metadata(&source_path).map_err(|e| {
      format!(
        "Unable to fetch filesystem metadata for {}. Details: {}",
        &source_path.to_string_lossy().code_str(),
        e
      )
    })?;

    // Check if the path is a directory.
    if metadata.is_dir() {
      // The path is a directory, so we need to traverse it.
      for entry in WalkDir::new(&source_path) {
        // If the user wants to stop the operation, quit now.
        if interrupted.load(Ordering::SeqCst) {
          return Err(super::INTERRUPT_MESSAGE.to_owned());
        }

        // Fetch the filesystem metadata for this entry.
        let entry = entry.map_err(|e| {
          format!(
            "Unable to traverse directory {}. Details: {}",
            &source_path.to_string_lossy().code_str(),
            e
          )
        })?;
        let entry_metadata = entry.metadata().map_err(|e| {
          format!(
            "Unable to fetch filesystem metadata for {}. Details: {}",
            &source_path.to_string_lossy().code_str(),
            e
          )
        })?;

        // If this entry is a file, add it to the archive.
        if entry.file_type().is_file() {
          add_file(
            &mut builder,
            &entry_metadata,
            entry
              .path()
              .canonicalize()
              .map_err(|e| {
                format!(
                  "Unable to canonicalize path {}. Details: {}",
                  &entry.path().to_string_lossy().code_str(),
                  e
                )
              })?
              .strip_prefix(&source_dir)
              .map_err(|e| {
                format!(
                  "Unable to relativize path {} with respect to {}. Details: {}",
                  &entry.path().to_string_lossy().code_str(),
                  &source_dir.to_string_lossy().code_str(),
                  e
                )
              })?,
            &source_dir,
            &destination_dir,
            &mut file_hashes,
          )?;
        }
      }
    } else {
      // The path is a file. Add it to the archive.
      add_file(
        &mut builder,
        &metadata,
        path,
        &source_dir,
        &destination_dir,
        &mut file_hashes,
      )?;
    }
  }

  // Sort the file hashes to ensure the directory traversal order doesn't
  // matter.
  file_hashes.sort();

  // Return the tar file and the hash of its contents.
  Ok((
    builder
      .into_inner()
      .map_err(|e| format!("Error writing tar archive. Details: {}", e))?,
    file_hashes
      .iter()
      .fold(cache::hash_str(""), |acc, x| cache::extend(&acc, x)),
  ))
}
