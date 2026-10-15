//! 状況画面(OVERVIEW / DETAILS)。5 でページを切り替える。
use crate::screen::{Outcome, RenderContext, Screen, ScreenAction, ScreenId};
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
