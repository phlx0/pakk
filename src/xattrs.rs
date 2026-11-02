use std::io;
use std::path::Path;

pub const PAX_PREFIX: &str = "SCHILY.xattr.";

pub type Attrs = Vec<(String, Vec<u8>)>;

#[cfg(unix)]
pub fn read(path: &Path) -> io::Result<Attrs> {
    let mut attrs = Attrs::new();
    for name in xattr::list(path)? {
        let Some(key) = name.to_str() else { continue };
        if let Some(value) = xattr::get(path, &name)? {
            attrs.push((format!("{PAX_PREFIX}{key}"), value));
        }
    }
    attrs.sort();
    Ok(attrs)
}

#[cfg(not(unix))]
pub fn read(_path: &Path) -> io::Result<Attrs> {
    Ok(Attrs::new())
}

#[cfg(unix)]
pub fn apply(path: &Path, attrs: &Attrs) {
    for (key, value) in attrs {
        if let Some(name) = key.strip_prefix(PAX_PREFIX) {
            let _ = xattr::set(path, name, value);
        }
    }
}

#[cfg(not(unix))]
pub fn apply(_path: &Path, _attrs: &Attrs) {}

pub fn from_entry<R: io::Read>(entry: &mut tar::Entry<'_, R>) -> io::Result<Attrs> {
    let mut attrs = Attrs::new();
    if let Some(extensions) = entry.pax_extensions()? {
        for extension in extensions {
            let extension = extension?;
            if let Ok(key) = extension.key()
                && key.starts_with(PAX_PREFIX)
            {
                attrs.push((key.to_owned(), extension.value_bytes().to_vec()));
            }
        }
    }
    Ok(attrs)
}
