//! VerticalPortal — 自定义垂直滚动容器 (constrain_horizontal = true)。

use xilem::masonry::widgets::Portal;
use xilem::core::{MessageContext, Mut, View, ViewMarker, MessageResult};
use xilem::{Pod, ViewCtx, WidgetView};

pub struct VerticalPortal<V, State, Action> {
    child: V,
    phantom: std::marker::PhantomData<(State, Action)>,
}

pub fn vertical_portal<State, Action, V>(child: V) -> VerticalPortal<V, State, Action>
where
    V: WidgetView<State, Action>,
{
    VerticalPortal {
        child,
        phantom: std::marker::PhantomData,
    }
}

impl<V, State, Action> ViewMarker for VerticalPortal<V, State, Action> {}

impl<Child, State, Action> View<State, Action, ViewCtx> for VerticalPortal<Child, State, Action>
where
    Child: WidgetView<State, Action>,
    State: 'static,
    Action: 'static,
{
    type Element = Pod<Portal<Child::Widget>>;
    type ViewState = Child::ViewState;

    fn build(&self, ctx: &mut ViewCtx, app_state: &mut State) -> (Self::Element, Self::ViewState) {
        let (child, child_state) = self.child.build(ctx, app_state);
        let widget_pod = ctx.create_pod(
            Portal::new(child.new_widget)
        );
        (widget_pod, child_state)
    }

    fn rebuild(
        &self,
        prev: &Self,
        view_state: &mut Self::ViewState,
        ctx: &mut ViewCtx,
        mut element: Mut<'_, Self::Element>,
        app_state: &mut State,
    ) {
        let child_element = Portal::child_mut(&mut element);
        self.child
            .rebuild(&prev.child, view_state, ctx, child_element, app_state);
    }

    fn teardown(
        &self,
        view_state: &mut Self::ViewState,
        ctx: &mut ViewCtx,
        mut element: Mut<'_, Self::Element>,
    ) {
        let child_element = Portal::child_mut(&mut element);
        self.child.teardown(view_state, ctx, child_element);
    }

    fn message(
        &self,
        view_state: &mut Self::ViewState,
        message: &mut MessageContext,
        mut element: Mut<'_, Self::Element>,
        app_state: &mut State,
    ) -> MessageResult<Action> {
        let child_element = Portal::child_mut(&mut element);
        self.child
            .message(view_state, message, child_element, app_state)
    }
}

pub struct HorizontalPortal<V, State, Action> {
    child: V,
    phantom: std::marker::PhantomData<(State, Action)>,
}

pub fn horizontal_portal<State, Action, V>(child: V) -> HorizontalPortal<V, State, Action>
where
    V: WidgetView<State, Action>,
{
    HorizontalPortal {
        child,
        phantom: std::marker::PhantomData,
    }
}

impl<V, State, Action> ViewMarker for HorizontalPortal<V, State, Action> {}

impl<Child, State, Action> View<State, Action, ViewCtx> for HorizontalPortal<Child, State, Action>
where
    Child: WidgetView<State, Action>,
    State: 'static,
    Action: 'static,
{
    type Element = Pod<Portal<Child::Widget>>;
    type ViewState = Child::ViewState;

    fn build(&self, ctx: &mut ViewCtx, app_state: &mut State) -> (Self::Element, Self::ViewState) {
        let (child, child_state) = self.child.build(ctx, app_state);
        let widget_pod = ctx.create_pod(
            Portal::new(child.new_widget)
        );
        (widget_pod, child_state)
    }

    fn rebuild(
        &self,
        prev: &Self,
        view_state: &mut Self::ViewState,
        ctx: &mut ViewCtx,
        mut element: Mut<'_, Self::Element>,
        app_state: &mut State,
    ) {
        let child_element = Portal::child_mut(&mut element);
        self.child
            .rebuild(&prev.child, view_state, ctx, child_element, app_state);
    }

    fn teardown(
        &self,
        view_state: &mut Self::ViewState,
        ctx: &mut ViewCtx,
        mut element: Mut<'_, Self::Element>,
    ) {
        let child_element = Portal::child_mut(&mut element);
        self.child.teardown(view_state, ctx, child_element);
    }

    fn message(
        &self,
        view_state: &mut Self::ViewState,
        message: &mut MessageContext,
        mut element: Mut<'_, Self::Element>,
        app_state: &mut State,
    ) -> MessageResult<Action> {
        let child_element = Portal::child_mut(&mut element);
        self.child
            .message(view_state, message, child_element, app_state)
    }
}
