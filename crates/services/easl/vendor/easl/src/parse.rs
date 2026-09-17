use std::{
  collections::{HashSet, VecDeque},
  io::Read,
  path::{Path, PathBuf},
  sync::LazyLock,
};

use fsexp::{
  Context as SSEContext, DocumentSyntaxTree, Encloser as SSEEncloser,
  EncloserOrOperator, Operator as SSEOperator, ParseError,
  document::{Document, DocumentPosition},
  standard_whitespace_chars,
  syntax::{ContextId, Syntax},
};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Context {
  Default,
  StructuredComment,
  UnstructuredComment,
  String,
}

impl ContextId for Context {
  fn is_comment(&self) -> bool {
    use Context::*;
    match self {
      StructuredComment | UnstructuredComment => true,
      _ => false,
    }
  }
}

use crate::compiler::{
  error::{CompileError, CompileErrorKind, ErrorLog},
  program::EaslDocument,
};
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Encloser {
  Parens,
  Square,
  Curly,
  LineComment,
  BlockComment,
  Quote,
}
impl SSEEncloser for Encloser {
  fn opening_encloser_str(&self) -> &str {
    use Encloser::*;
    match self {
      Parens => "(",
      Square => "[",
      Curly => "{",
      LineComment => ";",
      BlockComment => ";*",
      Quote => "\"",
    }
  }

  fn closing_encloser_str(&self) -> &str {
    use Encloser::*;
    match self {
      Parens => ")",
      Square => "]",
      Curly => "}",
      LineComment => "\n",
      BlockComment => "*;",
      Quote => "\"",
    }
  }
}
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Operator {
  Annotation,
  TypeAscription,
  ExpressionComment,
}
impl SSEOperator for Operator {
  fn left_args(&self) -> usize {
    match self {
      Operator::Annotation => 0,
      Operator::TypeAscription => 1,
      Operator::ExpressionComment => 0,
    }
  }

  fn right_args(&self) -> usize {
    match self {
      Operator::Annotation => 2,
      Operator::TypeAscription => 1,
      Operator::ExpressionComment => 1,
    }
  }

  fn op_str(&self) -> &str {
    match self {
      Operator::Annotation => "@",
      Operator::TypeAscription => ":",
      Operator::ExpressionComment => "#_",
    }
  }
}

static DEFAULT_CTX: LazyLock<SSEContext<Encloser, Operator>> =
  LazyLock::new(|| {
    SSEContext::new(
      vec![
        Encloser::Parens,
        Encloser::Square,
        Encloser::Curly,
        Encloser::LineComment,
        Encloser::BlockComment,
        Encloser::Quote,
      ],
      vec![
        Operator::Annotation,
        Operator::TypeAscription,
        Operator::ExpressionComment,
      ],
      None,
      standard_whitespace_chars(),
    )
  });

static TRIVIAL_CTX: LazyLock<SSEContext<Encloser, Operator>> =
  LazyLock::new(|| SSEContext::trivial());

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EaslSyntax;

impl Syntax for EaslSyntax {
  type C = Context;
  type E = Encloser;
  type O = Operator;

  fn root_context(&self) -> Self::C {
    Context::Default
  }
  fn context<'a>(&'a self, id: &Self::C) -> &'a SSEContext<Self::E, Self::O> {
    match id {
      Context::Default | Context::StructuredComment => &*DEFAULT_CTX,
      Context::UnstructuredComment | Context::String => &*TRIVIAL_CTX,
    }
  }
  fn encloser_context(&self, encloser: &Self::E) -> Option<Self::C> {
    match encloser {
      Encloser::LineComment | Encloser::BlockComment => {
        Some(Context::UnstructuredComment)
      }
      Encloser::Quote => Some(Context::String),
      _ => None,
    }
  }
  fn operator_context(&self, operator: &Self::O) -> Option<Self::C> {
    match operator {
      Operator::ExpressionComment => Some(Context::StructuredComment),
      _ => None,
    }
  }
  fn reserved_tokens(&self) -> impl Iterator<Item = &str> {
    ["||"].into_iter()
  }
}

pub fn parse_easl(easl_source: &str) -> EaslDocument {
  Document::from_text_with_syntax(EaslSyntax, easl_source)
}

pub fn parse_easl_without_comments(easl_source: &str) -> EaslDocument {
  let mut doc = parse_easl(easl_source);
  doc.strip_comments();
  doc
}

pub type EaslTree = DocumentSyntaxTree<Encloser, Operator>;

#[derive(Debug)]
pub struct EaslMultiDocument {
  pub sources: Vec<(EaslDocument, String, String)>,
}

impl EaslMultiDocument {
  fn empty() -> Self {
    Self { sources: vec![] }
  }
  pub fn from_singular_document_sourceless(document: EaslDocument) -> Self {
    Self::from_singular_document(document, String::new(), String::new())
  }
  pub fn from_singular_document(
    document: EaslDocument,
    path: String,
    source: String,
  ) -> Self {
    let mut docs = Self::empty();
    docs.add_document(document, path, source);
    docs
  }
  pub fn add_document(
    &mut self,
    mut document: EaslDocument,
    path: String,
    source: String,
  ) {
    for ast in document.syntax_trees.iter_mut() {
      ast.walk_mut(&mut |subast| {
        match subast {
          fsexp::Ast::Leaf(pos, _) | fsexp::Ast::Inner((pos, _), _) => {
            pos.path.insert(0, self.sources.len())
          }
        };
      });
    }
    self.sources.push((document, path, source));
  }
  pub fn describe_document_position(
    &self,
    mut pos: DocumentPosition,
  ) -> String {
    if pos.path.is_empty() {
      return "[INTERNAL CODE]".to_string();
    }
    let (source_document, source_path, source_text) =
      &self.sources[pos.path.remove(0)];
    let inner_pos_string =
      source_document.describe_document_position(pos.span, source_text);
    if source_path.is_empty() {
      inner_pos_string
    } else {
      format!("{}\n{}", source_path, inner_pos_string)
    }
  }
  pub fn describe_parse_error(&self, err: ParseError) -> String {
    let (source_document, _, source_text) = self.sources.last().unwrap();
    err.describe(source_document, source_text)
  }
}

/// Bounded source graph, shared by file compilation and in-memory editors.
#[derive(Clone, Copy, Debug)]
pub struct ImportLimits {
  pub documents: usize,
  pub utf8_bytes: usize,
}
impl Default for ImportLimits {
  fn default() -> Self {
    Self {
      documents: 128,
      utf8_bytes: 8 * 1024 * 1024,
    }
  }
}

/// Success, language/import failure with source context, or parse failure.
pub type ImportResult = Result<
  Result<EaslMultiDocument, (EaslMultiDocument, ErrorLog)>,
  EaslMultiDocument,
>;

/// A bounded local source read. Import resolution never writes the root or its
/// dependencies. A caller with another source authority can supply a lookup.
pub fn read_easl_source(
  path: &Path,
  max_bytes: usize,
) -> std::io::Result<String> {
  if !path.metadata()?.is_file() {
    return Err(std::io::Error::new(
      std::io::ErrorKind::InvalidInput,
      "EASL source must be a regular file",
    ));
  }
  let mut bytes = Vec::new();
  std::fs::File::open(path)?
    .take(max_bytes.saturating_add(1) as u64)
    .read_to_end(&mut bytes)?;
  if bytes.len() > max_bytes {
    return Err(std::io::Error::new(
      std::io::ErrorKind::InvalidData,
      "EASL source exceeds its byte limit",
    ));
  }
  String::from_utf8(bytes).map_err(|error| {
    std::io::Error::new(std::io::ErrorKind::InvalidData, error)
  })
}

/// Resolve imports around the supplied root buffer. The root need not exist on
/// disk; relative imports require its intended filename. Canonical dependency
/// identities deduplicate diamonds, cycles and symlink aliases. Root tree paths
/// are retained, so an editor can map diagnostics back to its unmodified AST.
pub fn load_easl_imports_with_lookup_function(
  document: EaslDocument,
  primary_path: Option<&Path>,
  source: String,
  limits: ImportLimits,
  mut lookup: impl FnMut(&Path) -> std::io::Result<String>,
) -> ImportResult {
  let mut documents = EaslMultiDocument::from_singular_document(
    document,
    primary_path
      .map(|path| path.to_string_lossy().into_owned())
      .unwrap_or_default(),
    source,
  );
  if !documents.sources[0].0.parsing_failures.is_empty() {
    return Err(documents);
  }
  let mut errors = ErrorLog::new();
  let mut total_bytes = documents.sources[0].2.len();
  if limits.documents == 0 || total_bytes > limits.utf8_bytes {
    errors.log(CompileError::new(
      CompileErrorKind::ImportLimitExceeded,
      crate::compiler::error::SourceTrace::empty(),
    ));
    return Ok(Err((documents, errors)));
  }
  let primary_identity = primary_path.and_then(|path| path.canonicalize().ok());
  let mut encountered = HashSet::new();
  if let Some(path) = primary_identity {
    encountered.insert(path);
  }
  let mut pending = VecDeque::new();
  let mut index = 0;
  let mut current_path = primary_path.map(PathBuf::from);
  loop {
    for ast in &documents.sources[index].0.syntax_trees {
      let EaslTree::Inner(
        (_, EncloserOrOperator::Encloser(Encloser::Parens)),
        children,
      ) = ast
      else {
        continue;
      };
      if !matches!(children.first(), Some(EaslTree::Leaf(_, name)) if name == "import")
      {
        continue;
      }
      let import_name = if children.len() == 2
        && let EaslTree::Inner(
          (_, EncloserOrOperator::Encloser(Encloser::Quote)),
          parts,
        ) = &children[1]
        && let [EaslTree::Leaf(_, name)] = parts.as_slice()
      {
        Some(name)
      } else {
        None
      };
      let Some(name) = import_name else {
        errors.log(CompileError::new(
          CompileErrorKind::InvalidImportStatement,
          ast.position().into(),
        ));
        continue;
      };
      let requested = Path::new(name);
      let resolved = if requested.is_absolute() {
        Ok(requested.to_owned())
      } else if let Some(parent) =
        current_path.as_ref().and_then(|path| path.parent())
      {
        Ok(parent.join(requested))
      } else {
        Err(std::io::Error::new(
          std::io::ErrorKind::InvalidInput,
          "Relative imports require a source filename; save the document or use an absolute import",
        ))
      };
      match resolved.and_then(|path| path.canonicalize()) {
        Ok(path) if encountered.contains(&path) => {}
        Ok(path) => {
          // Count the root independently: an in-memory root may not have a
          // filesystem identity to add to `encountered`.
          if documents.sources.len() + pending.len() >= limits.documents {
            errors.log(CompileError::new(
              CompileErrorKind::ImportLimitExceeded,
              ast.position().into(),
            ));
            break;
          }
          encountered.insert(path.clone());
          pending.push_back((path, ast.position().clone()));
        }
        Err(error) => errors.log(CompileError::new(
          CompileErrorKind::ImportSourceError(name.clone(), error.to_string()),
          ast.position().into(),
        )),
      }
    }
    if !errors.is_empty() {
      return Ok(Err((documents, errors)));
    }
    let Some((path, import_position)) = pending.pop_front() else {
      return Ok(Ok(documents));
    };
    let source = match lookup(&path) {
      Ok(source) => source,
      Err(error) => {
        errors.log(CompileError::new(
          CompileErrorKind::ImportSourceError(
            path.display().to_string(),
            error.to_string(),
          ),
          import_position.into(),
        ));
        return Ok(Err((documents, errors)));
      }
    };
    if source.len() > limits.utf8_bytes.saturating_sub(total_bytes) {
      errors.log(CompileError::new(
        CompileErrorKind::ImportLimitExceeded,
        import_position.into(),
      ));
      return Ok(Err((documents, errors)));
    }
    total_bytes += source.len();
    let parsed = parse_easl_without_comments(&source);
    documents.add_document(parsed, path.to_string_lossy().into_owned(), source);
    current_path = Some(path);
    index = documents.sources.len() - 1;
    if !documents.sources[index].0.parsing_failures.is_empty() {
      return Err(documents);
    }
  }
}

pub fn load_and_parse_easl_multidocument_with_lookup_function(
  primary_easl_file_path: &Path,
  mut lookup: impl FnMut(&Path) -> std::io::Result<String>,
) -> std::io::Result<ImportResult> {
  let path = primary_easl_file_path.canonicalize()?;
  let source = lookup(&path)?;
  let document = parse_easl_without_comments(&source);
  Ok(load_easl_imports_with_lookup_function(
    document,
    Some(&path),
    source,
    ImportLimits::default(),
    lookup,
  ))
}

pub fn load_and_parse_easl_multidocument(
  primary_easl_file_path: &Path,
) -> std::io::Result<
  Result<
    Result<EaslMultiDocument, (EaslMultiDocument, ErrorLog)>,
    EaslMultiDocument,
  >,
> {
  load_and_parse_easl_multidocument_with_lookup_function(
    primary_easl_file_path,
    |path| read_easl_source(path, ImportLimits::default().utf8_bytes),
  )
}
