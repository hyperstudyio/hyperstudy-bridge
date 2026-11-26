//! Pipelines made in the pipeline editor: openfrenz `config.json` files kept in Bridge's data folder, one folder
//! per pipeline (`<data>/pipelines/<name>/config.json`), so that they can be run like any openfrenz model, exported
//! as a folder, or uploaded to Hugging Face.
//!
//! A pipeline is checked by openfrenz before it is saved; a folder that does not hold one is never written.

use std::path::{Path, PathBuf};

use frenz_model::{Model, Pipeline, Take};
use serde::Serialize;
use serde_json::Value;

/// What the editor shows about a pipeline: whether openfrenz accepts it, what it gives, and what is wrong.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Check {
    pub ok: bool,
    /// Names of the values each step gives.
    pub outputs: Vec<String>,
    /// Streams read, with samples per step (0 for streams taken as they arrive).
    pub inputs: Vec<(String, usize)>,
    /// openfrenz's message when the pipeline is refused.
    pub error: Option<String>,
    /// The node the message is about, when it names one.
    pub node: Option<String>,
}

/// The file holding a model's stored numbers (trained weights, a calibration), when it has any.
pub const WEIGHTS: &str = "model.safetensors";

/// Builds the pipeline `config` describes. With `origin`, the folder of the model it was opened from, blocks
/// that need stored numbers read them from that model's `model.safetensors`.
pub fn build(config: &Value, origin: Option<&Path>) -> Result<Pipeline, frenz_model::Error> {
    let Some(weights) = origin.map(|o| o.join(WEIGHTS)).filter(|w| w.is_file()) else {
        return Pipeline::from_config(config);
    };
    // openfrenz reads stored numbers from a folder: a scratch one with this config and that model's numbers.
    let scratch =
        std::env::temp_dir().join(format!("hyperstudy-pipeline-{}", uuid::Uuid::new_v4()));
    let built = (|| {
        std::fs::create_dir_all(&scratch)?;
        std::fs::write(scratch.join("config.json"), serde_json::to_vec(config)?)?;
        if std::fs::hard_link(&weights, scratch.join(WEIGHTS)).is_err() {
            std::fs::copy(&weights, scratch.join(WEIGHTS))?;
        }
        Pipeline::from_dir(&scratch)
    })();
    let _ = std::fs::remove_dir_all(&scratch);
    built
}

/// Checks `config` as openfrenz would load it.
pub fn check(config: &Value) -> Check {
    check_with(config, None)
}

/// [`check`], with stored numbers from the model in `origin` (see [`build`]).
pub fn check_with(config: &Value, origin: Option<&Path>) -> Check {
    match build(config, origin) {
        Ok(pipeline) => {
            let spec = pipeline.spec();
            Check {
                ok: true,
                outputs: spec.outputs.clone(),
                inputs: spec
                    .inputs
                    .iter()
                    .map(|i| {
                        let samples = if let Take::Samples(n) = i.take { n } else { 0 };
                        (i.stream.clone(), samples)
                    })
                    .collect(),
                error: None,
                node: None,
            }
        }
        Err(error) => {
            let message = error.to_string();
            // openfrenz names a node as "node <name>: ..."
            let node = message.split("node ").nth(1).and_then(|rest| {
                rest.split(':')
                    .next()
                    .map(str::trim)
                    .filter(|n| !n.is_empty())
                    .map(str::to_owned)
            });
            let message = message
                .trim_start_matches("model files: ")
                .trim_start_matches("config.json: ")
                .to_owned();
            Check {
                ok: false,
                outputs: Vec::new(),
                inputs: Vec::new(),
                error: Some(message),
                node,
            }
        }
    }
}

/// A saved pipeline, as listed.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Saved {
    /// The folder's name, which is also how the FRENZ `models` setting names it (`saved:<name>`).
    pub name: String,
    pub version: String,
    pub outputs: Vec<String>,
    /// Seconds since 1970 of the last save.
    pub saved_at: f64,
    pub config: Value,
}

/// The folder all saved pipelines live in.
pub fn root(data_dir: &Path) -> PathBuf {
    data_dir.join("pipelines")
}

/// Whether `name` can name a saved pipeline: letters, digits, `-` and `_`, at most 64 of them.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn folder(data_dir: &Path, name: &str) -> Result<PathBuf, String> {
    if valid_name(name) {
        Ok(root(data_dir).join(name))
    } else {
        Err("A pipeline's name may hold letters, digits, - and _ only (at most 64).".into())
    }
}

/// Whether `dir` is a saved pipeline's folder; its name if so.
pub fn saved_name(data_dir: &Path, dir: &Path) -> Option<String> {
    let root = root(data_dir).canonicalize().ok()?;
    let dir = dir.canonicalize().ok()?;
    if dir.parent()? != root {
        return None;
    }
    let name = dir.file_name()?.to_str()?.to_owned();
    valid_name(&name).then_some(name)
}

/// The folder of the saved pipeline `name`, if there is one.
pub fn path(data_dir: &Path, name: &str) -> Result<PathBuf, String> {
    let dir = folder(data_dir, name)?;
    if dir.join("config.json").is_file() {
        Ok(dir)
    } else {
        Err(format!("There is no saved pipeline called {name}."))
    }
}

/// Saves `config` as the pipeline `name`, after openfrenz has accepted it. Its `name` is set to `name`. With
/// `origin`, the model it was opened from, that model's stored numbers are saved with it.
pub fn save(
    data_dir: &Path,
    name: &str,
    config: &Value,
    origin: Option<&Path>,
) -> Result<Check, String> {
    let dir = folder(data_dir, name)?;
    let mut config = config.clone();
    if let Some(object) = config.as_object_mut() {
        object.insert("name".into(), Value::String(name.to_owned()));
        object
            .entry("version")
            .or_insert_with(|| Value::String("1".into()));
        object.insert("openfrenz_model".into(), Value::String("pipeline".into()));
    } else {
        return Err("A pipeline is a JSON object.".into());
    }
    let checked = check_with(&config, origin);
    if !checked.ok {
        return Err(checked
            .error
            .unwrap_or_else(|| "openfrenz refused it".into()));
    }
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    if let Some(weights) = origin.map(|o| o.join(WEIGHTS)).filter(|w| w.is_file()) {
        let target = dir.join(WEIGHTS);
        let same =
            matches!((weights.canonicalize(), target.canonicalize()), (Ok(a), Ok(b)) if a == b);
        if !same {
            let partial = dir.join("model.safetensors.partial");
            std::fs::copy(&weights, &partial).map_err(|e| e.to_string())?;
            std::fs::rename(&partial, &target).map_err(|e| e.to_string())?;
        }
    }
    let text = serde_json::to_string_pretty(&config).map_err(|e| e.to_string())?;
    // Written whole and then renamed, so that a crash never leaves half a file.
    let partial = dir.join("config.json.partial");
    std::fs::write(&partial, text).map_err(|e| e.to_string())?;
    std::fs::rename(&partial, dir.join("config.json")).map_err(|e| e.to_string())?;
    Ok(checked)
}

/// Every saved pipeline that openfrenz still accepts, newest first.
pub fn list(data_dir: &Path) -> Vec<Saved> {
    let Ok(entries) = std::fs::read_dir(root(data_dir)) else {
        return Vec::new();
    };
    let mut found: Vec<Saved> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_str()?.to_owned();
            if !valid_name(&name) {
                return None;
            }
            let file = entry.path().join("config.json");
            let config: Value = serde_json::from_str(&std::fs::read_to_string(&file).ok()?).ok()?;
            let saved_at = std::fs::metadata(&file)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0.0, |d| d.as_secs_f64());
            let checked = check_with(&config, Some(&entry.path()));
            Some(Saved {
                version: config
                    .get("version")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                outputs: checked.outputs,
                name,
                saved_at,
                config,
            })
        })
        .collect();
    found.sort_by(|a, b| b.saved_at.total_cmp(&a.saved_at));
    found
}

/// Deletes the saved pipeline `name`.
pub fn delete(data_dir: &Path, name: &str) -> Result<(), String> {
    let dir = path(data_dir, name)?;
    std::fs::remove_dir_all(dir).map_err(|e| e.to_string())
}

/// Copies the saved pipeline `name` into `destination/<name>/`, ready to share or upload. Returns that folder.
pub fn export(data_dir: &Path, name: &str, destination: &Path) -> Result<PathBuf, String> {
    let dir = path(data_dir, name)?;
    let out = destination.join(name);
    if out.exists() {
        return Err(format!("{} already exists.", out.display()));
    }
    std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    std::fs::copy(dir.join("config.json"), out.join("config.json")).map_err(|e| e.to_string())?;
    if dir.join(WEIGHTS).is_file() {
        std::fs::copy(dir.join(WEIGHTS), out.join(WEIGHTS)).map_err(|e| e.to_string())?;
    }
    std::fs::write(out.join("README.md"), readme(name, &dir)?).map_err(|e| e.to_string())?;
    Ok(out)
}

/// A short model card for an exported or uploaded pipeline.
pub fn readme(name: &str, dir: &Path) -> Result<String, String> {
    let config: Value = serde_json::from_str(
        &std::fs::read_to_string(dir.join("config.json")).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let checked = check_with(&config, Some(dir));
    let outputs = checked
        .outputs
        .iter()
        .map(|o| format!("- `{o}`"))
        .collect::<Vec<_>>()
        .join("\n");
    let inputs = checked
        .inputs
        .iter()
        .map(|(stream, samples)| format!("`{stream}` ({samples} samples a step)"))
        .collect::<Vec<_>>()
        .join(", ");
    Ok(format!(
        "---\nlibrary_name: openfrenz\ntags: [eeg, wearable, frenz]\n---\n\n# {name}\n\n\
An openfrenz pipeline made in the HyperStudy Bridge pipeline editor. Reads {inputs}.\n\n\
```python\nimport openfrenz\nmodel = openfrenz.Pipeline(\"<owner>/{name}\")\n```\n\nGives, every step:\n\n{outputs}\n"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn config() -> Value {
        json!({
            "step": { "stream": "eeg", "samples": 125 },
            "nodes": [
                { "name": "front", "op": "select", "in": "eeg", "channels": ["forehead_left", "forehead_right"] },
                { "name": "level", "op": "summary", "in": "front", "stats": ["sd"] }
            ],
            "outputs": ["level"],
            "editor": { "front": [0, 0], "level": [0, 120] }
        })
    }

    #[test]
    fn a_pipeline_is_checked_saved_listed_exported_and_deleted() {
        let data = std::env::temp_dir().join(format!("pipelines-test-{}", uuid::Uuid::new_v4()));
        let mut wrong = config();
        wrong["openfrenz_model"] = json!("pipeline");
        let checked = check(&{
            let mut c = wrong.clone();
            c["nodes"][1]["stats"] = json!(["loudness"]);
            c
        });
        assert!(!checked.ok);
        assert_eq!(checked.node.as_deref(), Some("level"));
        assert!(checked.error.unwrap().starts_with("node level"));

        assert!(save(&data, "bad name!", &config(), None).is_err());
        let saved = save(&data, "front-level", &config(), None).unwrap();
        assert_eq!(saved.outputs, ["forehead_left:sd", "forehead_right:sd"]);
        assert_eq!(saved.inputs, [("eeg".to_owned(), 125)]);
        let listed = list(&data);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "front-level");
        assert_eq!(listed[0].config["name"], "front-level");
        assert_eq!(listed[0].config["editor"]["level"], json!([0, 120]));

        let out = std::env::temp_dir().join(format!("pipelines-export-{}", uuid::Uuid::new_v4()));
        let exported = export(&data, "front-level", &out).unwrap();
        assert!(exported.join("config.json").is_file());
        let card = std::fs::read_to_string(exported.join("README.md")).unwrap();
        assert!(card.contains("`forehead_left:sd`") && card.contains("`eeg` (125 samples a step)"));
        assert!(
            export(&data, "front-level", &out).is_err(),
            "an export never overwrites"
        );

        // A pipeline openfrenz refuses is never written.
        let mut refused = config();
        refused["outputs"] = json!(["nowhere"]);
        assert!(save(&data, "refused", &refused, None).is_err());
        assert!(path(&data, "refused").is_err());

        delete(&data, "front-level").unwrap();
        assert!(list(&data).is_empty());
        let _ = std::fs::remove_dir_all(&data);
        let _ = std::fs::remove_dir_all(&out);
    }
}
