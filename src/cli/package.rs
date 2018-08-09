use base64;
use error::DefaultResult;
use serde_json::{self, Map, Value};
use std::{
    fs::{self, File},
    io::{Read, Write},
    path::PathBuf,
};

pub const DEFAULT_BUNDLE_FILE_NAME: &str = "bundle.json";

pub const META_FILE_ID: &str = "file";
pub const META_DIR_ID: &str = "dir";

pub const META_SECTION_NAME: &str = "__META__";
pub const META_TREE_SECTION_NAME: &str = "tree";
pub const META_CONFIG_SECTION_NAME: &str = "config_file";

pub type Object = Map<String, Value>;

pub fn package(strip_meta: bool, output: Option<PathBuf>) -> DefaultResult<()> {
    let output = output.unwrap_or(PathBuf::from(DEFAULT_BUNDLE_FILE_NAME));

    let dir_obj_bundle = bundle_recurse(PathBuf::from("."), strip_meta)?;

    let out_file = File::create(&output)?;

    serde_json::to_writer_pretty(&out_file, &Value::Object(dir_obj_bundle))?;

    println!("Wrote bundle file to {:?}", output);

    Ok(())
}

fn bundle_recurse(path: PathBuf, strip_meta: bool) -> DefaultResult<Object> {
    let root: Vec<_> = path
        .read_dir()?
        .filter(|e| e.is_ok())
        .map(|e| e.unwrap().path())
        .collect();

    let maybe_json_file_path = root
        .iter()
        .filter(|e| e.is_file())
        .find(|e| e.to_str().unwrap().ends_with(".json"));

    // Scan files but discard found json file
    let all_nodes = root.iter().filter(|node_path| {
        maybe_json_file_path
            .and_then(|path| Some(node_path != &path))
            .unwrap_or(true)
    });

    let mut meta_section = Object::new();

    // Obtain the config file
    let mut main_tree: Object = if let Some(json_file_path) = maybe_json_file_path {
        let file_name = json_file_path
            .file_name()
            .ok_or_else(|| format_err!("unable to retrieve file name"))?;

        let file_name = file_name
            .to_str()
            .ok_or_else(|| format_err!("unable to retrieve file name"))?;

        meta_section.insert(
            META_CONFIG_SECTION_NAME.into(),
            Value::String(file_name.into()),
        );

        let json_file = fs::read_to_string(json_file_path)?;

        serde_json::from_str(&json_file)?
    } else {
        Object::new()
    };

    // Let's go meta. Way meta!
    let mut meta_tree = Object::new();

    for node in all_nodes {
        let file_name = node
            .file_name()
            .ok_or_else(|| format_err!("unable to retrieve file name"))?;

        let file_name = file_name
            .to_str()
            .ok_or_else(|| format_err!("unable to retrieve file name"))?;

        if node.is_file() {
            meta_tree.insert(file_name.into(), Value::String(META_FILE_ID.into()));

            let mut buf = Vec::new();
            File::open(node)?.read_to_end(&mut buf)?;
            let encoded_content = base64::encode(&buf);

            main_tree.insert(file_name.into(), Value::String(encoded_content));
        } else if node.is_dir() {
            meta_tree.insert(file_name.into(), Value::String(META_DIR_ID.into()));

            let sub_tree_content = bundle_recurse(node.clone(), strip_meta)?;

            main_tree.insert(file_name.into(), Value::Object(sub_tree_content));
        }
    }

    if !strip_meta {
        if meta_tree.len() > 0 {
            meta_section.insert(META_TREE_SECTION_NAME.into(), Value::Object(meta_tree));
        }

        if meta_section.len() > 0 {
            main_tree.insert(META_SECTION_NAME.into(), Value::Object(meta_section));
        }
    }

    Ok(main_tree)
}

pub fn unpack(path: PathBuf, to: PathBuf) -> DefaultResult<()> {
    ensure!(path.is_file(), "'path' doesn't point ot a file");
    ensure!(to.is_dir(), "'to' doesn't point ot a directory");

    if !to.exists() {
        fs::create_dir_all(&to)?;
    }

    let raw_bundle_content = fs::read_to_string(&path)?;
    let bundle_content: Object = serde_json::from_str(&raw_bundle_content)?;

    unpack_recurse(bundle_content, to)?;

    Ok(())
}

fn unpack_recurse(mut obj: Object, to: PathBuf) -> DefaultResult<()> {
    if let Some(Value::Object(mut main_meta_obj)) = obj.remove(META_SECTION_NAME) {
        // unpack the tree
        if let Some(Value::Object(tree_meta_obj)) = main_meta_obj.remove(META_TREE_SECTION_NAME) {
            for (meta_entry, meta_value) in tree_meta_obj {
                let entry = obj
                    .remove(&meta_entry)
                    .ok_or_else(|| format_err!("incompatible meta section"))?;

                if let Value::String(node_type) = meta_value {
                    match node_type.as_str() {
                        META_FILE_ID if entry.is_string() => {
                            let base64_content = entry.as_str().unwrap().to_string();
                            let content = base64::decode(&base64_content)?;

                            File::create(to.join(meta_entry))?.write_all(&content[..])?;
                        }
                        META_DIR_ID if entry.is_object() => {
                            let directory_obj = entry.as_object().unwrap();
                            let dir_path = to.join(meta_entry);

                            fs::create_dir(dir_path.clone())?;

                            unpack_recurse(directory_obj.clone(), dir_path.clone())?;
                        }
                        _ => bail!("incompatible meta section"),
                    }
                } else {
                    bail!("incompatible meta section");
                }
            }
        }

        // unpack the config file
        if let Some(config_file_meta) = main_meta_obj.remove(META_CONFIG_SECTION_NAME) {
            ensure!(
                config_file_meta.is_string(),
                "config file has to be a string"
            );

            if obj.len() > 0 {
                let dna_file = File::create(to.join(config_file_meta.as_str().unwrap()))?;
                serde_json::to_writer_pretty(dna_file, &obj)?;
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_cmd::prelude::*;
    use dir_diff;
    use std::process::Command;
    use tempfile::{Builder, TempDir};

    const HOLOCHAIN_TEST_PREFIX: &str = "org.holochain.test";

    fn gen_dir() -> TempDir {
        Builder::new()
            .prefix(HOLOCHAIN_TEST_PREFIX)
            .tempdir()
            .unwrap()
    }

    #[test]
    fn package_and_unpack_isolated() {
        const DEFAULT_BUNDLE_FILE_NAME: &str = "bundle.json";

        fn package(shared_file_path: &PathBuf) {
            let temp_space = gen_dir();
            let temp_dir_path = temp_space.path();

            Command::main_binary()
                .unwrap()
                .args(&["init", temp_dir_path.to_str().unwrap()])
                .assert()
                .success();

            let bundle_file_path = shared_file_path.join(DEFAULT_BUNDLE_FILE_NAME);

            Command::main_binary()
                .unwrap()
                .args(&["package", "-o", bundle_file_path.to_str().unwrap()])
                .current_dir(&temp_dir_path)
                .assert()
                .success();
        }

        fn unpack(shared_file_path: &PathBuf) {
            let temp_space = gen_dir();
            let temp_dir_path = temp_space.path();

            Command::main_binary()
                .unwrap()
                .current_dir(&shared_file_path)
                .args(&[
                    "unpack",
                    DEFAULT_BUNDLE_FILE_NAME,
                    temp_dir_path.to_str().unwrap(),
                ])
                .assert()
                .success();
        }

        let shared_space = gen_dir();

        package(&shared_space.path().to_path_buf());

        unpack(&shared_space.path().to_path_buf());

        shared_space.close().unwrap();
    }

    #[test]
    /// A test ensuring that packaging and unpacking a project results in the very same project
    fn package_reverse() {
        const DEFAULT_BUNDLE_FILE_NAME: &str = "bundle.json";

        const SOURCE_DIR_NAME: &str = "source_app";
        const DEST_DIR_NAME: &str = "dest_app";

        let shared_space = gen_dir();

        let root_path = shared_space.path().to_path_buf();

        let source_path = shared_space.path().join(SOURCE_DIR_NAME);
        fs::create_dir_all(&source_path).unwrap();

        // Initialize and package a project
        Command::main_binary()
            .unwrap()
            .args(&["init", source_path.to_str().unwrap()])
            .assert()
            .success();

        let bundle_file_path = root_path.join(DEFAULT_BUNDLE_FILE_NAME);

        Command::main_binary()
            .unwrap()
            .args(&["package", "-o", bundle_file_path.to_str().unwrap()])
            .current_dir(&source_path)
            .assert()
            .success();

        // Unpack the project from the generated bundle
        let dest_path = shared_space.path().join(DEST_DIR_NAME);
        fs::create_dir_all(&dest_path).unwrap();

        Command::main_binary()
            .unwrap()
            .args(&[
                "unpack",
                bundle_file_path.to_str().unwrap(),
                dest_path.to_str().unwrap(),
            ])
            .assert()
            .success();

        // Assert for equality
        assert!(!dir_diff::is_different(&source_path, &dest_path).unwrap());
    }
}