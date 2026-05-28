//! HoverWidget + Hoverable View — 自定义 hover 检测 widget。

use std::marker::PhantomData;

use xilem::masonry::core::{
    BoxConstraints, EventCtx, LayoutCtx, PaintCtx, RegisterCtx, AccessCtx,
    Widget, WidgetPod, PointerEvent, PropertiesMut, PropertiesRef,
    ChildrenIds,
};
use xilem::masonry::kurbo::Size;
use xilem::masonry::vello::Scene;
use xilem::masonry::accesskit::{Node, Role};

use xilem::core::{MessageContext, Mut, View, ViewMarker, MessageResult, ViewId, ViewPathTracker};
use xilem::{Pod, ViewCtx, WidgetView};

// ===== Low-level Widget =====

pub struct HoverWidget {
    child: WidgetPod<dyn Widget>,
    was_hovered: bool,
}

impl HoverWidget {
    pub fn new(child: xilem::masonry::core::NewWidget<impl Widget + ?Sized>) -> Self {
        Self {
            child: child.erased().to_pod(),
            was_hovered: false,
        }
    }

    pub fn child_mut<'t>(this: &'t mut xilem::masonry::core::WidgetMut<'_, Self>) -> xilem::masonry::core::WidgetMut<'t, dyn Widget> {
        let child = &mut this.widget.child;
        this.ctx.get_mut(child)
    }
}

impl Widget for HoverWidget {
    type Action = bool;

    fn accepts_pointer_interaction(&self) -> bool {
        true
    }

    fn register_children(&mut self, ctx: &mut RegisterCtx<'_>) {
        ctx.register_child(&mut self.child);
    }

    fn layout(
        &mut self,
        ctx: &mut LayoutCtx<'_>,
        _props: &mut PropertiesMut<'_>,
        bc: &BoxConstraints,
    ) -> Size {
        let size = ctx.run_layout(&mut self.child, bc);
        ctx.place_child(&mut self.child, xilem::masonry::kurbo::Point::ORIGIN);
        size
    }

    fn paint(&mut self, _ctx: &mut PaintCtx<'_>, _props: &PropertiesRef<'_>, _scene: &mut Scene) {}

    fn accessibility_role(&self) -> Role {
        Role::GenericContainer
    }

    fn accessibility(&mut self, _ctx: &mut AccessCtx<'_>, _props: &PropertiesRef<'_>, _node: &mut Node) {}

    fn children_ids(&self) -> ChildrenIds {
        ChildrenIds::from_slice(&[self.child.id()])
    }

    fn on_pointer_event(
        &mut self,
        ctx: &mut EventCtx<'_>,
        _props: &mut PropertiesMut<'_>,
        event: &PointerEvent,
    ) {
        let is_hovered = ctx.is_hovered();
        tracing::debug!("HoverWidget event: is_hovered={}, was_hovered={}, event={:?}", is_hovered, self.was_hovered, event);
        
        // Also support standard enter/leave/cancel in case they are generated
        match event {
            PointerEvent::Enter(_) => {
                tracing::debug!("HoverWidget: matched Enter");
                if !self.was_hovered {
                    self.was_hovered = true;
                    ctx.submit_action::<bool>(true);
                }
            }
            PointerEvent::Leave(_) | PointerEvent::Cancel(_) => {
                tracing::debug!("HoverWidget: matched Leave/Cancel");
                if self.was_hovered {
                    self.was_hovered = false;
                    ctx.submit_action::<bool>(false);
                }
            }
            _ => {
                // If standard events aren't generated or are missed, check state changes directly
                if is_hovered != self.was_hovered {
                    tracing::debug!("HoverWidget state changed: from {} to {}", self.was_hovered, is_hovered);
                    self.was_hovered = is_hovered;
                    ctx.submit_action::<bool>(is_hovered);
                }
            }
        }
    }
}

// ===== Hoverable View Wrapper =====

pub struct Hoverable<V, F, State, Action> {
    child: V,
    callback: F,
    phantom: PhantomData<fn() -> (State, Action)>,
}

pub fn hoverable<State, Action, V>(
    child: V,
    callback: impl Fn(&mut State, bool) -> Action + Send + Sync + 'static,
) -> Hoverable<V, impl for<'a> Fn(&'a mut State, bool) -> MessageResult<Action> + Send + Sync + 'static, State, Action>
where
    V: WidgetView<State, Action>,
{
    Hoverable {
        child,
        callback: move |state: &mut State, hovered| MessageResult::Action(callback(state, hovered)),
        phantom: PhantomData,
    }
}

const HOVER_CONTENT_VIEW_ID: ViewId = ViewId::new(0);

impl<V, F, State, Action> ViewMarker for Hoverable<V, F, State, Action> {}
impl<F, V, State, Action> View<State, Action, ViewCtx> for Hoverable<V, F, State, Action>
where
    V: WidgetView<State, Action>,
    F: Fn(&mut State, bool) -> MessageResult<Action> + Send + Sync + 'static,
    State: 'static,
    Action: 'static,
{
    type Element = Pod<HoverWidget>;
    type ViewState = V::ViewState;

    fn build(&self, ctx: &mut ViewCtx, app_state: &mut State) -> (Self::Element, Self::ViewState) {
        let (child, child_state) = ctx.with_id(HOVER_CONTENT_VIEW_ID, |ctx| {
            View::<State, Action, _>::build(&self.child, ctx, app_state)
        });
        (
            ctx.with_action_widget(|ctx| {
                ctx.create_pod(HoverWidget::new(child.new_widget))
            }),
            child_state,
        )
    }

    fn rebuild(
        &self,
        prev: &Self,
        state: &mut Self::ViewState,
        ctx: &mut ViewCtx,
        mut element: Mut<'_, Self::Element>,
        app_state: &mut State,
    ) {
        ctx.with_id(HOVER_CONTENT_VIEW_ID, |ctx| {
            let mut child = HoverWidget::child_mut(&mut element);
            View::<State, Action, _>::rebuild(
                &self.child,
                &prev.child,
                state,
                ctx,
                child.downcast(),
                app_state,
            );
        });
    }

    fn teardown(
        &self,
        view_state: &mut Self::ViewState,
        ctx: &mut ViewCtx,
        mut element: Mut<'_, Self::Element>,
    ) {
        ctx.with_id(HOVER_CONTENT_VIEW_ID, |ctx| {
            let mut child = HoverWidget::child_mut(&mut element);
            View::<State, Action, _>::teardown(
                &self.child,
                view_state,
                ctx,
                child.downcast(),
            );
        });
        ctx.teardown_leaf(element);
    }

    fn message(
        &self,
        view_state: &mut Self::ViewState,
        message: &mut MessageContext,
        mut element: Mut<'_, Self::Element>,
        app_state: &mut State,
    ) -> MessageResult<Action> {
        match message.take_first() {
            Some(HOVER_CONTENT_VIEW_ID) => {
                let mut child = HoverWidget::child_mut(&mut element);
                self.child.message(
                    view_state,
                    message,
                    child.downcast(),
                    app_state,
                )
            }
            None => match message.take_message::<bool>() {
                Some(hovered) => (self.callback)(app_state, *hovered),
                None => MessageResult::Stale,
            },
            _ => MessageResult::Stale,
        }
    }
}


