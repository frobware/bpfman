//! Definition-name suggestions for literal bind heads.

pub(super) fn nearest_definition<'a>(
    head: &str,
    definitions: impl Iterator<Item = &'a str>,
) -> Option<&'a str> {
    let head: Vec<char> = head.chars().collect();
    let limit = if head.len() <= 3 {
        1
    } else {
        (head.len() / 2).max(3)
    };
    definitions
        .filter_map(|definition| {
            let distance = edit_distance(&head, definition);
            (distance <= limit).then_some((distance, definition))
        })
        .min()
        .map(|(_, definition)| definition)
}

fn edit_distance(head: &[char], definition: &str) -> usize {
    let mut row: Vec<_> = (0..=head.len()).collect();
    for (index, ch) in definition.chars().enumerate() {
        let mut diagonal = row[0];
        row[0] = index + 1;
        for (index, target) in head.iter().enumerate() {
            let previous = row[index + 1];
            row[index + 1] = (previous + 1)
                .min(row[index] + 1)
                .min(diagonal + usize::from(ch != *target));
            diagonal = previous;
        }
    }
    row[head.len()]
}
