use super::recipe_licence_lines;

#[test]
fn the_licence_lines_of_a_recipe_are_listed_with_their_numbers() {
    let spec = "Name: ffmpeg-8\nLicense:        LGPL-2.1-or-later\n# --enable-gpl is a comment\n%if %{with gpl}\n\t--enable-gpl \\\n%endif\n\t--enable-version3 \\\n\t--enable-shared \\\n";
    let lines = recipe_licence_lines(spec);
    assert_eq!(
        lines,
        [
            "  line 2: License:        LGPL-2.1-or-later",
            "  line 4: %if %{with gpl}",
            "  line 5: --enable-gpl \\",
            "  line 7: --enable-version3 \\",
        ]
    );
}
