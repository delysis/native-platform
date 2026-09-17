use std::sync::LazyLock;

use crate::{
  Context, ContextId, Encloser, standard_whitespace_chars,
  syntax::{NoOperator, Syntax},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SexpWithStringsContext {
  Default,
  InsideString,
}

impl ContextId for SexpWithStringsContext {
  fn is_comment(&self) -> bool {
    false
  }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SexpWithStringsEncloser {
  Parens,
  Quotes,
}

impl Encloser for SexpWithStringsEncloser {
  fn opening_encloser_str(&self) -> &str {
    match self {
      SexpWithStringsEncloser::Parens => "(",
      SexpWithStringsEncloser::Quotes => "\"",
    }
  }

  fn closing_encloser_str(&self) -> &str {
    match self {
      SexpWithStringsEncloser::Parens => ")",
      SexpWithStringsEncloser::Quotes => "\"",
    }
  }
}

static DEFAULT_CTX: LazyLock<Context<SexpWithStringsEncloser, NoOperator>> =
  LazyLock::new(|| {
    Context::new(
      vec![
        SexpWithStringsEncloser::Parens,
        SexpWithStringsEncloser::Quotes,
      ],
      vec![],
      None,
      standard_whitespace_chars(),
    )
  });

static INSIDE_STRING_CTX: LazyLock<
  Context<SexpWithStringsEncloser, NoOperator>,
> = LazyLock::new(|| {
  Context::new(vec![], vec![], Some('\\'.to_string()), vec![])
});

pub struct SexpWithStringsSyntax;
impl Syntax for SexpWithStringsSyntax {
  type C = SexpWithStringsContext;
  type E = SexpWithStringsEncloser;
  type O = NoOperator;

  fn root_context(&self) -> SexpWithStringsContext {
    SexpWithStringsContext::Default
  }

  fn context<'a>(&'a self, ctx: &Self::C) -> &'a Context<Self::E, Self::O> {
    match ctx {
      SexpWithStringsContext::Default => &DEFAULT_CTX,
      SexpWithStringsContext::InsideString => &INSIDE_STRING_CTX,
    }
  }

  fn encloser_context(&self, encloser: &Self::E) -> Option<Self::C> {
    match encloser {
      SexpWithStringsEncloser::Parens => None,
      SexpWithStringsEncloser::Quotes => {
        Some(SexpWithStringsContext::InsideString)
      }
    }
  }

  fn operator_context(&self, _: &Self::O) -> Option<Self::C> {
    None
  }
}

#[cfg(test)]
mod pseudo_clj_tests {
  use crate::{
    Parser, SyntaxTree,
    examples::sexp_with_strings::{
      SexpWithStringsEncloser::{self, *},
      SexpWithStringsSyntax,
    },
    syntax::{EncloserOrOperator, NoOperator},
  };

  fn leaf(s: &str) -> SyntaxTree<SexpWithStringsEncloser, NoOperator> {
    SyntaxTree::Leaf((), s.to_string())
  }
  fn inner(
    encloser: SexpWithStringsEncloser,
    subexpressions: Vec<SyntaxTree<SexpWithStringsEncloser, NoOperator>>,
  ) -> SyntaxTree<SexpWithStringsEncloser, NoOperator> {
    SyntaxTree::Inner(EncloserOrOperator::Encloser(encloser), subexpressions)
  }

  #[test]
  fn basic() {
    assert_eq!(
      Parser::new(SexpWithStringsSyntax, "(a b \"c\")")
        .read_next()
        .0
        .map(SyntaxTree::from),
      Some(inner(
        Parens,
        vec![leaf("a"), leaf("b"), inner(Quotes, vec![leaf("c")])]
      ))
    )
  }

  #[test]
  fn multi_word_string() {
    assert_eq!(
      Parser::new(
        SexpWithStringsSyntax,
        "(f \"this is just one long expression, not multiple\")"
      )
      .read_next()
      .0
      .map(SyntaxTree::from),
      Some(inner(
        Parens,
        vec![
          leaf("f"),
          inner(
            Quotes,
            vec![leaf("this is just one long expression, not multiple")]
          )
        ]
      ))
    )
  }

  #[test]
  fn string_delimiters_do_not_absorb_unicode_grapheme_parts() {
    for contents in [
      "\u{11f02}", // Prepend: would join the closing quote as one grapheme.
      "\u{600}",
      "\u{301}a", // Extend: would join the opening quote.
      "\u{200d}x",
      "👩‍👩‍👧‍👦\r\nकु",
      "a\\\"\u{301}b",
    ] {
      let source = format!("(f \"{contents}\")");
      let (tree, errors) =
        Parser::new(SexpWithStringsSyntax, &source).read_next();
      assert!(errors.is_empty(), "{contents:?}: {errors:?}");
      assert_eq!(
        tree.map(SyntaxTree::from),
        Some(inner(
          Parens,
          vec![leaf("f"), inner(Quotes, vec![leaf(contents)])]
        )),
        "{contents:?}"
      );
    }
  }

  #[test]
  fn unbalanced_parens_in_quotes() {
    assert_eq!(
      Parser::new(SexpWithStringsSyntax, "(f (g \")))\"))")
        .read_next()
        .0
        .map(SyntaxTree::from),
      Some(inner(
        Parens,
        vec![
          leaf("f"),
          inner(Parens, vec![leaf("g"), inner(Quotes, vec![leaf(")))")])])
        ]
      ))
    )
  }
}
