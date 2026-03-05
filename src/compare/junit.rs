use std::io::Write;
use std::path::Path;

use anyhow::Result;

use super::threshold::ThresholdResult;

/// Write JUnit XML test report from threshold results.
pub fn write_junit_xml(path: &Path, results: &[ThresholdResult], elapsed_secs: f64) -> Result<()> {
    let failures = results.iter().filter(|r| !r.passed).count();
    let mut buf = Vec::new();

    writeln!(buf, "<?xml version=\"1.0\" encoding=\"UTF-8\"?>")?;
    writeln!(
        buf,
        "<testsuites tests=\"{}\" failures=\"{}\" time=\"{:.3}\">",
        results.len(),
        failures,
        elapsed_secs
    )?;
    writeln!(
        buf,
        "  <testsuite name=\"pg-retest\" tests=\"{}\" failures=\"{}\">",
        results.len(),
        failures
    )?;

    for result in results {
        if result.passed {
            writeln!(
                buf,
                "    <testcase name=\"{}\" time=\"{:.3}\"/>",
                xml_escape(&result.name),
                result.actual / 1000.0
            )?;
        } else {
            writeln!(
                buf,
                "    <testcase name=\"{}\" time=\"{:.3}\">",
                xml_escape(&result.name),
                result.actual / 1000.0
            )?;
            let msg = result.message.as_deref().unwrap_or("threshold exceeded");
            writeln!(buf, "      <failure message=\"{}\"/>", xml_escape(msg))?;
            writeln!(buf, "    </testcase>")?;
        }
    }

    writeln!(buf, "  </testsuite>")?;
    writeln!(buf, "</testsuites>")?;

    std::fs::write(path, buf)?;
    Ok(())
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
