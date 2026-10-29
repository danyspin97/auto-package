use super::*;

#[test]
fn the_description_gets_the_note_once() {
    let spec = "%description\nNCCL runtime\n\n%prep\n";
    let out = note_prebuilt_library(spec, true);
    assert!(
        out.starts_with("%description\nNCCL runtime\n\nThis package redistributes the vendor's")
    );
    assert_eq!(out.matches("redistributes").count(), 1);
    assert!(out.contains("%prep"));
}

#[test]
fn the_note_goes_after_the_whole_description_paragraph() {
    let spec = "%description\nNCCL runtime, the\nwrapped second line,\nand a third.\n\n%prep\n";
    let out = note_prebuilt_library(spec, false);
    assert!(
            out.starts_with(
                "%description\nNCCL runtime, the\nwrapped second line,\nand a third.\n\nThis package redistributes"
            ),
            "{out}"
        );
    assert!(
        out.ends_with("unpacked from its wheel; it is not built from source.\n\n%prep\n"),
        "{out}"
    );
}

#[test]
fn nvidia_licence_text_is_proprietary() {
    assert!(license_is_proprietary(&License::FreeText(
        "LicenseRef-NVIDIA-Proprietary".into()
    )));
    assert!(license_is_proprietary(&License::Spdx(
        NVIDIA_PROPRIETARY.into()
    )));
    assert!(!license_is_proprietary(&License::Spdx("MIT".into())));
    assert!(!license_is_proprietary(&License::Unknown));
}
