use crate::error::{Context, JavelinError, Result};
use crate::model::ViewMarker;
use std::fs;
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone)]
pub struct ProjectContext {
    pub root: PathBuf,
    pub metadata: PathBuf,
    pub layer_id: String,
    pub view: PathBuf,
}

pub fn validate_relative(path: &str) -> Result<()> {
    if path.is_empty() || path.contains('\0') || path.contains('\\') {
        return Err(JavelinError::unsupported(format!(
            "unsafe or unsupported path {path:?}"
        )));
    }
    let value = Path::new(path);
    if value.is_absolute()
        || value.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
        || path == ".javelin"
        || path.starts_with(".javelin/")
        || path == ".javelin-view"
    {
        return Err(JavelinError::unsupported(format!("unsafe path {path:?}")));
    }
    Ok(())
}

pub fn discover(start: Option<&Path>) -> Result<ProjectContext> {
    let requested = start.unwrap_or_else(|| Path::new("."));
    let absolute = if requested.is_absolute() {
        requested.to_path_buf()
    } else {
        std::env::current_dir()
            .jctx("PATH_IO", "cannot read current directory")?
            .join(requested)
    };
    let absolute = absolute
        .canonicalize()
        .jctx("PATH_IO", format!("cannot resolve {}", absolute.display()))?;
    let start_directory = if absolute.is_file() {
        absolute.parent().unwrap_or(&absolute).to_path_buf()
    } else {
        absolute
    };

    for directory in start_directory.ancestors() {
        let marker_path = directory.join(".javelin-view");
        if marker_path.is_file() {
            let bytes = fs::read(&marker_path).jctx(
                "VIEW_MARKER",
                format!("cannot read {}", marker_path.display()),
            )?;
            let marker: ViewMarker = serde_json::from_slice(&bytes).map_err(|error| {
                JavelinError::corruption(format!("invalid view marker: {error}"))
            })?;
            let root = PathBuf::from(marker.project)
                .canonicalize()
                .jctx("VIEW_MARKER", "view marker project does not exist")?;
            return Ok(ProjectContext {
                metadata: root.join(".javelin"),
                root,
                layer_id: marker.layer_id,
                view: directory.to_path_buf(),
            });
        }
        if directory.join(".javelin/store.sqlite3").is_file() {
            return Ok(ProjectContext {
                root: directory.to_path_buf(),
                metadata: directory.join(".javelin"),
                layer_id: "local".into(),
                view: directory.to_path_buf(),
            });
        }
    }
    Err(JavelinError::no_world(start_directory.display()))
}

pub fn safe_join(root: &Path, relative: &str) -> Result<PathBuf> {
    validate_relative(relative)?;
    let mut current = root.to_path_buf();
    let components: Vec<_> = Path::new(relative).components().collect();
    for (index, component) in components.iter().enumerate() {
        let Component::Normal(name) = component else {
            return Err(JavelinError::unsupported(format!(
                "unsafe path {relative:?}"
            )));
        };
        current.push(name);
        if index + 1 < components.len()
            && current
                .symlink_metadata()
                .is_ok_and(|m| m.file_type().is_symlink())
        {
            return Err(JavelinError::unsupported(format!(
                "symlink parent rejected for {relative:?}"
            )));
        }
    }
    Ok(current)
}
