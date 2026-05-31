
use xilem::masonry::core::{
    BoxConstraints, LayoutCtx, PaintCtx, RegisterCtx, AccessCtx,
    Widget, WidgetPod, PropertiesMut, PropertiesRef,
    ChildrenIds,
};
use xilem::masonry::kurbo::{Size, Point};
use xilem::masonry::vello::Scene;
use xilem::masonry::accesskit::{Node, Role};

use xilem::core::{MessageContext, Mut, View, ViewMarker, MessageResult, ViewId, ViewPathTracker};
use xilem::{Pod, ViewCtx, WidgetView};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AnchorPoint {
    TopLeft,
    TopCenter,
    TopRight,
    LeftCenter,
    Center,
    RightCenter,
    BottomLeft,
    BottomCenter,
    BottomRight,
    Custom(f64, f64),
    CustomCenterRelative(f64, f64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PopoverAlign {
    TopLeft,
    TopCenter,
    TopRight,
    LeftCenter,
    Center,
    RightCenter,
    BottomLeft,
    BottomCenter,
    BottomRight,
}

#[derive(Debug, Clone, Copy)]
pub struct PopoverConfig {
    pub anchor_point: AnchorPoint,
    pub popover_align: PopoverAlign,
    pub offset_x: f64,
    pub offset_y: f64,
}

impl Default for PopoverConfig {
    fn default() -> Self {
        Self {
            anchor_point: AnchorPoint::BottomLeft,
            popover_align: PopoverAlign::TopLeft,
            offset_x: 0.0,
            offset_y: 4.0,
        }
    }
}

pub struct PopoverStackWidget {
    main: WidgetPod<dyn Widget>,
    overlay: WidgetPod<dyn Widget>,
    config: PopoverConfig,
}

impl PopoverStackWidget {
    pub fn new(
        main: xilem::masonry::core::NewWidget<impl Widget + ?Sized>,
        overlay: xilem::masonry::core::NewWidget<impl Widget + ?Sized>,
        config: PopoverConfig,
    ) -> Self {
        Self {
            main: main.erased().to_pod(),
            overlay: overlay.erased().to_pod(),
            config,
        }
    }

    pub fn set_config(this: &mut xilem::masonry::core::WidgetMut<'_, Self>, config: PopoverConfig) {
        this.widget.config = config;
        this.ctx.request_layout();
    }

    pub fn main_mut<'t>(this: &'t mut xilem::masonry::core::WidgetMut<'_, Self>) -> xilem::masonry::core::WidgetMut<'t, dyn Widget> {
        let child = &mut this.widget.main;
        this.ctx.get_mut(child)
    }

    pub fn overlay_mut<'t>(this: &'t mut xilem::masonry::core::WidgetMut<'_, Self>) -> xilem::masonry::core::WidgetMut<'t, dyn Widget> {
        let child = &mut this.widget.overlay;
        this.ctx.get_mut(child)
    }
}

impl Widget for PopoverStackWidget {
    type Action = ();

    fn accepts_pointer_interaction(&self) -> bool {
        true
    }

    fn register_children(&mut self, ctx: &mut RegisterCtx<'_>) {
        ctx.register_child(&mut self.main);
        ctx.register_child(&mut self.overlay);
    }

    fn layout(
        &mut self,
        ctx: &mut LayoutCtx<'_>,
        _props: &mut PropertiesMut<'_>,
        bc: &BoxConstraints,
    ) -> Size {
        // Lay out anchor (main child) with original constraints
        let main_size = ctx.run_layout(&mut self.main, bc);
        ctx.place_child(&mut self.main, Point::ORIGIN);

        // Measure overlay naturally under loose constraints
        let loose_bc = bc.loosen();
        let overlay_size = ctx.run_layout(&mut self.overlay, &loose_bc);

        let w_m = main_size.width;
        let h_m = main_size.height;
        let w_o = overlay_size.width;
        let h_o = overlay_size.height;

        // 1. Calculate Anchor Point coordinate
        let (a_x, a_y) = match self.config.anchor_point {
            AnchorPoint::TopLeft => (0.0, 0.0),
            AnchorPoint::TopCenter => (w_m / 2.0, 0.0),
            AnchorPoint::TopRight => (w_m, 0.0),
            AnchorPoint::LeftCenter => (0.0, h_m / 2.0),
            AnchorPoint::Center => (w_m / 2.0, h_m / 2.0),
            AnchorPoint::RightCenter => (w_m, h_m / 2.0),
            AnchorPoint::BottomLeft => (0.0, h_m),
            AnchorPoint::BottomCenter => (w_m / 2.0, h_m),
            AnchorPoint::BottomRight => (w_m, h_m),
            AnchorPoint::Custom(cx, cy) => (cx, cy),
            AnchorPoint::CustomCenterRelative(cx, cy) => (w_m / 2.0 + cx, cy),
        };

        // 2. Calculate Popover Alignment Point offset on overlay
        let (p_x, p_y) = match self.config.popover_align {
            PopoverAlign::TopLeft => (0.0, 0.0),
            PopoverAlign::TopCenter => (w_o / 2.0, 0.0),
            PopoverAlign::TopRight => (w_o, 0.0),
            PopoverAlign::LeftCenter => (0.0, h_o / 2.0),
            PopoverAlign::Center => (w_o / 2.0, h_o / 2.0),
            PopoverAlign::RightCenter => (w_o, h_o / 2.0),
            PopoverAlign::BottomLeft => (0.0, h_o),
            PopoverAlign::BottomCenter => (w_o / 2.0, h_o),
            PopoverAlign::BottomRight => (w_o, h_o),
        };

        // 3. Compute final popover top-left coordinate (Anchor + Offset - PopoverAlign)
        let x = a_x + self.config.offset_x - p_x;
        let y = a_y + self.config.offset_y - p_y;

        ctx.place_child(&mut self.overlay, Point::new(x, y));

        main_size
    }

    fn paint(&mut self, _ctx: &mut PaintCtx<'_>, _props: &PropertiesRef<'_>, _scene: &mut Scene) {}

    fn accessibility_role(&self) -> Role {
        Role::GenericContainer
    }

    fn accessibility(&mut self, _ctx: &mut AccessCtx<'_>, _props: &PropertiesRef<'_>, _node: &mut Node) {}

    fn children_ids(&self) -> ChildrenIds {
        ChildrenIds::from_slice(&[self.main.id(), self.overlay.id()])
    }


}

pub struct PopoverStack<V1, V2> {
    main: V1,
    overlay: V2,
    config: PopoverConfig,
}

pub fn popover_stack<State, Action, V1, V2>(
    main: V1,
    overlay: V2,
    config: PopoverConfig,
) -> PopoverStack<V1, V2>
where
    V1: WidgetView<State, Action>,
    V2: WidgetView<State, Action>,
{
    PopoverStack { main, overlay, config }
}

const POPOVER_STACK_MAIN_VIEW_ID: ViewId = ViewId::new(0);
const POPOVER_STACK_OVERLAY_VIEW_ID: ViewId = ViewId::new(1);

impl<V1, V2> ViewMarker for PopoverStack<V1, V2> {}
impl<V1, V2, State, Action> View<State, Action, ViewCtx> for PopoverStack<V1, V2>
where
    V1: WidgetView<State, Action>,
    V2: WidgetView<State, Action>,
    State: 'static,
    Action: 'static,
{
    type Element = Pod<PopoverStackWidget>;
    type ViewState = (V1::ViewState, V2::ViewState);

    fn build(&self, ctx: &mut ViewCtx, app_state: &mut State) -> (Self::Element, Self::ViewState) {
        let (main, main_state) = ctx.with_id(POPOVER_STACK_MAIN_VIEW_ID, |ctx| {
            View::<State, Action, _>::build(&self.main, ctx, app_state)
        });
        let (overlay, overlay_state) = ctx.with_id(POPOVER_STACK_OVERLAY_VIEW_ID, |ctx| {
            View::<State, Action, _>::build(&self.overlay, ctx, app_state)
        });
        (
            ctx.create_pod(PopoverStackWidget::new(main.new_widget, overlay.new_widget, self.config)),
            (main_state, overlay_state),
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
        PopoverStackWidget::set_config(&mut element, self.config);
        ctx.with_id(POPOVER_STACK_MAIN_VIEW_ID, |ctx| {
            let mut child = PopoverStackWidget::main_mut(&mut element);
            View::<State, Action, _>::rebuild(
                &self.main,
                &prev.main,
                &mut state.0,
                ctx,
                child.downcast(),
                app_state,
            );
        });
        ctx.with_id(POPOVER_STACK_OVERLAY_VIEW_ID, |ctx| {
            let mut child = PopoverStackWidget::overlay_mut(&mut element);
            View::<State, Action, _>::rebuild(
                &self.overlay,
                &prev.overlay,
                &mut state.1,
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
        ctx.with_id(POPOVER_STACK_MAIN_VIEW_ID, |ctx| {
            let mut child = PopoverStackWidget::main_mut(&mut element);
            View::<State, Action, _>::teardown(
                &self.main,
                &mut view_state.0,
                ctx,
                child.downcast(),
            );
        });
        ctx.with_id(POPOVER_STACK_OVERLAY_VIEW_ID, |ctx| {
            let mut child = PopoverStackWidget::overlay_mut(&mut element);
            View::<State, Action, _>::teardown(
                &self.overlay,
                &mut view_state.1,
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
            Some(POPOVER_STACK_MAIN_VIEW_ID) => {
                let mut child = PopoverStackWidget::main_mut(&mut element);
                self.main.message(
                    &mut view_state.0,
                    message,
                    child.downcast(),
                    app_state,
                )
            }
            Some(POPOVER_STACK_OVERLAY_VIEW_ID) => {
                let mut child = PopoverStackWidget::overlay_mut(&mut element);
                self.overlay.message(
                    &mut view_state.1,
                    message,
                    child.downcast(),
                    app_state,
                )
            }
            _ => MessageResult::Stale,
        }
    }
}
