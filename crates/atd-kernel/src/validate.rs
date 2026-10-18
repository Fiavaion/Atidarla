//! Validates generated WGSL with naga's front-end and validator (the checks wgpu runs),
//! and maps a failure back to the kernel source through the source map.

use crate::diag::{Error, Span};
use crate::lower::SourceMap;

/// `Ok(())` if naga accepts `wgsl`; otherwise the error, placed in the kernel.
pub(crate) fn validate(wgsl: &str, map: &SourceMap) -> Result<(), Error> {
    let place = |span: Option<naga::Span>, message: String| -> Error {
        let range = span.and_then(|s| s.to_range());
        let kernel = range.as_ref().and_then(|r| map.kernel_span(r.start));
        match kernel {
            Some(k) => Error::new(k, message),
            None => {
                let line = range
                    .map(|r| wgsl[..r.start.min(wgsl.len())].matches('\n').count() + 1)
                    .unwrap_or(0);
                Error::new(
                    Span::default(),
                    format!("internal compiler error: the generated WGSL is invalid (line {line}): {message}"),
                )
                .hint("this is a bug in the kernel compiler; please report the kernel")
            }
        }
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
        let span = e.spans().map(|(s, _)| *s).find(|s| s.is_defined());
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
