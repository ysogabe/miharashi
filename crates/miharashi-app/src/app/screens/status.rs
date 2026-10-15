//! 状況画面(OVERVIEW / DETAILS)。5 でページを切り替える。
use crate::screen::{KeyLabels, Outcome, RenderContext, Screen, ScreenAction, ScreenId};
use miharashi_render::screens::{self, StatusPage};
use std::time::Instant;

pub struct StatusScreen {
    /// 表示中のページ。ほかの画面から戻ると、最後に見ていたページになる
    page: StatusPage,
}

impl StatusScreen {
    pub fn new() -> Self {
        Self {
            page: StatusPage::Overview,
        }
    }
}

impl Screen for StatusScreen {
    fn id(&self) -> ScreenId {
        "status"
    }

    fn title(&self) -> &'static str {
        "STATUS"
    }

    fn handle_action(&mut self, action: ScreenAction, _now: Instant) -> Outcome {
        match action {
            ScreenAction::Select => {
                self.page = self.page.toggled();
                Outcome::Consumed
            }
            ScreenAction::Up | ScreenAction::Down => Outcome::Ignored,
        }
    }

    fn key_labels(&self) -> KeyLabels {
        KeyLabels {
            select: Some(match self.page {
                StatusPage::Overview => "詳細",
                StatusPage::Details => "概要",
            }),
            ..KeyLabels::default()
        }
    }

    fn render(&mut self, ctx: &mut RenderContext<'_>) {
        screens::status(ctx.canvas, ctx.text, &ctx.world.system, self.page);
    }

    fn page(&self) -> Option<&'static str> {
        Some(match self.page {
            StatusPage::Overview => "OVERVIEW",
            StatusPage::Details => "DETAILS",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 5 は概要と詳細の切り替えで、ラベルは今と反対のページ。8 / 2 は使わない。
    #[test]
    fn key_labels_match_actions() {
        let mut s = StatusScreen::new();
        assert_eq!(s.key_labels().select, Some("詳細"));
        assert_eq!((s.key_labels().up, s.key_labels().down), (None, None));
        s.handle_action(ScreenAction::Select, Instant::now());
        assert_eq!(s.key_labels().select, Some("概要"));
        crate::screen::assert_key_labels_match_actions(&mut s, Instant::now());
    }
}
