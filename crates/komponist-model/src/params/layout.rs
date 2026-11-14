//! The auto-layout of a foreign plugin's device card: 4 columns of knobs, the plugin's macros if
//! it reports any, else its first 12 parameters.

use super::ParamInfo;

/// Columns of the auto-layout grid.
pub const AUTO_COLUMNS: usize = 4;
/// Knobs of the auto-layout (3 rows of [`AUTO_COLUMNS`]).
pub const AUTO_MAX: usize = 12;

/// Which of `infos` (already without hidden and read-only parameters) the auto-layout shows, as
/// indices in display order: the macros when any is reported, else the first parameters; at most
/// [`AUTO_MAX`] either way.
pub fn auto_layout(infos: &[ParamInfo]) -> Vec<usize> {
    let macros: Vec<usize> = (0..infos.len()).filter(|&i| infos[i].is_macro).collect();
    let mut picked = if macros.is_empty() {
        (0..infos.len()).collect()
    } else {
        macros
    };
    picked.truncate(AUTO_MAX);
    picked
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::ParamKind;

    fn infos(n: u32, macros: &[u32]) -> Vec<ParamInfo> {
        (0..n)
            .map(|i| {
                let mut p = ParamInfo::new(
                    crate::params::ParamId::plugin(i),
                    format!("P{i}"),
                    ParamKind::lin(0.0, 1.0),
                    0.0,
                );
                p.is_macro = macros.contains(&i);
                p
            })
            .collect()
    }

    #[test]
    fn the_first_twelve_without_macros() {
        assert_eq!(auto_layout(&infos(40, &[])), (0..12).collect::<Vec<_>>());
        assert_eq!(auto_layout(&infos(5, &[])), (0..5).collect::<Vec<_>>());
        assert!(auto_layout(&[]).is_empty());
    }

    #[test]
    fn macros_win_over_position() {
        assert_eq!(auto_layout(&infos(40, &[30, 3, 20])), [3, 20, 30]);
        let many: Vec<u32> = (10..40).collect();
        assert_eq!(auto_layout(&infos(40, &many)), (10..22).collect::<Vec<_>>());
    }
}
