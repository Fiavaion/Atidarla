//! Validates generated WGSL with naga's front-end and validator (the checks wgpu runs),
//! and maps a failure back to the kernel source through the source map.
//!
//! The checker is meant to reject everything naga would, so a failure here is a
//! compiler bug. It is still reported at the kernel construct that produced the WGSL, so
//! the user can see what to change while the bug is fixed.

use crate::diag::Error;
use crate::lower::SourceMap;

/// `Ok(())` if naga accepts `wgsl`; otherwise the error, placed in the kernel.
pub(crate) fn validate(wgsl: &str, map: &SourceMap) -> Result<(), Error> {
    let place = |span: Option<naga::Span>, message: String| -> Error {
        let range = span.and_then(|s| s.to_range());
        let line = range.as_ref().map_or(0, |r| {
            wgsl[..r.start.min(wgsl.len())].matches('\n').count() + 1
        });
        let kernel = range.as_ref().and_then(|r| map.kernel_span(r.start));
        Error::new(
            kernel.unwrap_or_default(),
            format!("the generated WGSL is invalid (WGSL line {line}): {message}"),
        )
        .hint("this is a bug in the kernel compiler; please report the kernel")
    };
    let module = naga::front::wgsl::parse_str(wgsl).map_err(|e| {
        let span = e
            .labels()
            .map(|(s, _)| s)
            .find(|s| s.to_range().is_some_and(|r| !r.is_empty()));
        place(span, e.message().to_owned())
    })?;
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::default(),
    )
    .validate(&module)
    .map_err(|e| {
        // The most specific span: naga also lists the enclosing function.
        let span = e
            .spans()
            .map(|(s, _)| *s)
            .filter(|s| s.is_defined())
            .min_by_key(|s| s.to_range().map_or(usize::MAX, |r| r.len()));
        let mut message = e.as_inner().to_string();
        let mut source = std::error::Error::source(e.as_inner());
        while let Some(s) = source {
            message.push_str(": ");
            message.push_str(&s.to_string());
            source = s.source();
        }
        place(span, message)
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diag::Span;

    #[test]
    fn errors_map_to_the_kernel_span() {
        let wgsl = "fn f() -> i32 {\n    return 1u;\n}\n";
        let at = wgsl.find("1u").unwrap_or(0);
        let map = SourceMap {
            entries: vec![
                (0..wgsl.len(), Span::new(0, 30)),
                (at..at + 2, Span::new(40, 42)),
            ],
        };
        let e = validate(wgsl, &map).expect_err("type mismatch");
        assert_eq!(e.span, Span::new(40, 42), "{e:?}");
        assert!(e.message.contains("WGSL line 2"), "{e:?}");
        assert!(e.hint.as_deref().unwrap_or("").contains("bug"));
        // Unmapped: the start of the kernel.
        let e = validate("fn f( {", &SourceMap::default()).expect_err("syntax");
        assert_eq!(e.span, Span::default());
        assert!(validate("fn f() -> i32 { return 1i; }", &SourceMap::default()).is_ok());
    }
}
