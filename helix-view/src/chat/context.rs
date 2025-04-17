use std::{ops::RangeBounds, path::Path};

use arc_swap::access::{DynAccess, DynGuard};
use helix_core::{find_workspace, syntax::Loader};

use crate::{DocumentId, Editor};

fn generate_content_block(content: &str, range: impl RangeBounds<usize>) -> String {
    if content.len() == 0 {
        return "1:".to_owned();
    }
    content
        .lines()
        .enumerate()
        .filter(|(i, _)| range.contains(i))
        .map(|(i, line)| format!("{}: {line}\n", i + 1))
        .collect()
}

fn path_relative_to_root(path: &Path) -> &Path {
    let root = find_workspace().0;
    path.strip_prefix(root).unwrap_or(path)
}

#[derive(Debug)]
pub enum Context {
    Document { name: DocumentId },
    Selection,
}

impl Context {
    pub fn document(doc: DocumentId) -> Self {
        Self::Document { name: doc }
    }

    pub fn resolve(&self, editor: &Editor) -> Result<String, anyhow::Error> {
        let (provider, arg) = match self {
            Context::Document { name } => {
                let doc = editor.document(*name).unwrap();
                let Some(path) = doc.path() else {
                    anyhow::bail!("file context only supports named files");
                };
                let path = path_relative_to_root(path);
                (FILE_CONTEXT, path.to_string_lossy().into_owned())
            }
            Context::Selection => (SELECTION_CONTEXT, "".to_owned()),
        };
        (provider.prepare)(editor, &arg)
    }
}

pub struct ContextProvider {
    pub name: &'static str,
    pub description: &'static str,
    pub prepare: fn(&Editor, &str) -> Result<String, anyhow::Error>,
}

pub const FILE_CONTEXT: &ContextProvider = &ContextProvider {
    name: "file",
    description: "Includes content of provided file in chat context. Supports input.",
    prepare: |ed, arg| {
        let root = find_workspace().0;
        let path = Path::new(arg);
        let full_path = if path.is_relative() {
            root.join(path)
        } else {
            path.to_owned()
        };
        let doc = ed.document_by_path(&full_path);
        let (doc_text, file_type) = match doc {
            Some(doc) => (
                doc.text().to_string(),
                doc.language_name().unwrap_or("").to_owned(),
            ),
            None => {
                let syn: DynGuard<Loader> = ed.syn_loader.load();
                let lang = match syn.language_config_for_file_name(&full_path) {
                    Some(lc) => lc.language_id.as_str().to_owned(),
                    None => "".to_owned(),
                };
                (std::fs::read_to_string(&full_path)?, lang)
            }
        };

        let doc_text = generate_content_block(&doc_text, ..);
        let name = path.to_string_lossy();

        let text = format!("# FILE:{name} CONTEXT\n```{file_type}\n{doc_text}\n```\n\n");

        Ok(text)
    },
};

pub const SELECTION_CONTEXT: &ContextProvider = &ContextProvider {
    name: "selection",
    description: "Includes content of currently selected text by the user.",
    prepare: |ed, _arg| {
        let (view, doc) = current_ref!(ed);
        let sel = doc.selection(view.id).primary();
        let relative_path = doc
            .path()
            .map_or_else(|| "".into(), |p| path_relative_to_root(p).to_string_lossy());

        let (start, end) = sel.line_range(doc.text().slice(..));

        let file_type = doc.language_name().unwrap_or("");
        let doc_text = generate_content_block(&doc.text().to_string(), start..=end);
        let text = format!(
            "# FILE:{relative_path} CONTEXT\nUser's active selection:\n```{file_type}\n{doc_text}\n```\n\n"
        );
        Ok(text)
    },
};
