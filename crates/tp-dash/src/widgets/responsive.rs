//! ResponsiveLayout — 自适应空间分辨率容器。
//! 根据外部传入的最大宽度约束 (bc.max().width) 是否达到指定阈值，
//! 动态选用宽屏视图 (wide) 或窄屏视图 (narrow) 进行排版与定位。

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

// ===== Low-level ResponsiveLayoutWidget =====

pub struct ResponsiveLayoutWidget {
    wide: WidgetPod<dyn Widget>,
    narrow: WidgetPod<dyn Widget>,
    threshold: f64,
}

impl ResponsiveLayoutWidget {
    pub fn new(
        wide: xilem::masonry::core::NewWidget<impl Widget + ?Sized>,
        narrow: xilem::masonry::core::NewWidget<impl Widget + ?Sized>,
        threshold: f64,
    ) -> Self {
        Self {
            wide: wide.erased().to_pod(),
            narrow: narrow.erased().to_pod(),
            threshold,
        }
    }

    pub fn set_threshold(this: &mut xilem::masonry::core::WidgetMut<'_, Self>, threshold: f64) {
        if this.widget.threshold != threshold {
            this.widget.threshold = threshold;
            this.ctx.request_layout();
        }
    }

    pub fn wide_mut<'t>(this: &'t mut xilem::masonry::core::WidgetMut<'_, Self>) -> xilem::masonry::core::WidgetMut<'t, dyn Widget> {
        let child = &mut this.widget.wide;
        this.ctx.get_mut(child)
    }

    pub fn narrow_mut<'t>(this: &'t mut xilem::masonry::core::WidgetMut<'_, Self>) -> xilem::masonry::core::WidgetMut<'t, dyn Widget> {
        let child = &mut this.widget.narrow;
        this.ctx.get_mut(child)
    }
}

impl Widget for ResponsiveLayoutWidget {
    type Action = ();

    fn accepts_pointer_interaction(&self) -> bool {
        true
    }

    fn register_children(&mut self, ctx: &mut RegisterCtx<'_>) {
        ctx.register_child(&mut self.wide);
        ctx.register_child(&mut self.narrow);
    }

    fn layout(
        &mut self,
        ctx: &mut LayoutCtx<'_>,
        _props: &mut PropertiesMut<'_>,
        bc: &BoxConstraints,
    ) -> Size {
        // 根据当前的可用最大宽度判断是否属于宽屏空间
        let is_wide = bc.max().width >= self.threshold;

        if is_wide {
            // 宽屏模式：布局宽屏子节点
            let size = ctx.run_layout(&mut self.wide, bc);
            ctx.place_child(&mut self.wide, Point::ORIGIN);

            // 窄屏模式组件收缩为零并移出屏幕隐藏，避免触发鼠标交互或多余绘制
            let zero_bc = BoxConstraints::tight(Size::ZERO);
            let _ = ctx.run_layout(&mut self.narrow, &zero_bc);
            ctx.place_child(&mut self.narrow, Point::new(-9999.0, -9999.0));

            size
        } else {
            // 窄屏模式：布局窄屏子节点
            let size = ctx.run_layout(&mut self.narrow, bc);
            ctx.place_child(&mut self.narrow, Point::ORIGIN);

            // 宽屏模式组件收缩为零并移出屏幕隐藏
            let zero_bc = BoxConstraints::tight(Size::ZERO);
            let _ = ctx.run_layout(&mut self.wide, &zero_bc);
            ctx.place_child(&mut self.wide, Point::new(-9999.0, -9999.0));

            size
        }
    }

    fn paint(&mut self, _ctx: &mut PaintCtx<'_>, _props: &PropertiesRef<'_>, _scene: &mut Scene) {}

    fn accessibility_role(&self) -> Role {
        Role::GenericContainer
    }

    fn accessibility(&mut self, _ctx: &mut AccessCtx<'_>, _props: &PropertiesRef<'_>, _node: &mut Node) {}

    fn children_ids(&self) -> ChildrenIds {
        ChildrenIds::from_slice(&[self.wide.id(), self.narrow.id()])
    }


}

// ===== Xilem View wrapper for ResponsiveLayoutWidget =====

pub struct ResponsiveLayout<V1, V2> {
    wide: V1,
    narrow: V2,
    threshold: f64,
}

pub fn responsive_layout<State, Action, V1, V2>(
    wide: V1,
    narrow: V2,
    threshold: f64,
) -> ResponsiveLayout<V1, V2>
where
    V1: WidgetView<State, Action>,
    V2: WidgetView<State, Action>,
{
    ResponsiveLayout { wide, narrow, threshold }
}

const RESPONSIVE_LAYOUT_WIDE_VIEW_ID: ViewId = ViewId::new(0);
const RESPONSIVE_LAYOUT_NARROW_VIEW_ID: ViewId = ViewId::new(1);

impl<V1, V2> ViewMarker for ResponsiveLayout<V1, V2> {}

impl<V1, V2, State, Action> View<State, Action, ViewCtx> for ResponsiveLayout<V1, V2>
where
    V1: WidgetView<State, Action>,
    V2: WidgetView<State, Action>,
    State: 'static,
    Action: 'static,
{
    type Element = Pod<ResponsiveLayoutWidget>;
    type ViewState = (V1::ViewState, V2::ViewState);

    fn build(&self, ctx: &mut ViewCtx, app_state: &mut State) -> (Self::Element, Self::ViewState) {
        let (wide, wide_state) = ctx.with_id(RESPONSIVE_LAYOUT_WIDE_VIEW_ID, |ctx| {
            View::<State, Action, _>::build(&self.wide, ctx, app_state)
        });
        let (narrow, narrow_state) = ctx.with_id(RESPONSIVE_LAYOUT_NARROW_VIEW_ID, |ctx| {
            View::<State, Action, _>::build(&self.narrow, ctx, app_state)
        });
        (
            ctx.create_pod(ResponsiveLayoutWidget::new(wide.new_widget, narrow.new_widget, self.threshold)),
            (wide_state, narrow_state),
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
        ResponsiveLayoutWidget::set_threshold(&mut element, self.threshold);
        ctx.with_id(RESPONSIVE_LAYOUT_WIDE_VIEW_ID, |ctx| {
            let mut child = ResponsiveLayoutWidget::wide_mut(&mut element);
            View::<State, Action, _>::rebuild(
                &self.wide,
                &prev.wide,
                &mut state.0,
                ctx,
                child.downcast(),
                app_state,
            );
        });
        ctx.with_id(RESPONSIVE_LAYOUT_NARROW_VIEW_ID, |ctx| {
            let mut child = ResponsiveLayoutWidget::narrow_mut(&mut element);
            View::<State, Action, _>::rebuild(
                &self.narrow,
                &prev.narrow,
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
        ctx.with_id(RESPONSIVE_LAYOUT_WIDE_VIEW_ID, |ctx| {
            let mut child = ResponsiveLayoutWidget::wide_mut(&mut element);
            View::<State, Action, _>::teardown(
                &self.wide,
                &mut view_state.0,
                ctx,
                child.downcast(),
            );
        });
        ctx.with_id(RESPONSIVE_LAYOUT_NARROW_VIEW_ID, |ctx| {
            let mut child = ResponsiveLayoutWidget::narrow_mut(&mut element);
            View::<State, Action, _>::teardown(
                &self.narrow,
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
            Some(RESPONSIVE_LAYOUT_WIDE_VIEW_ID) => {
                let mut child = ResponsiveLayoutWidget::wide_mut(&mut element);
                self.wide.message(
                    &mut view_state.0,
                    message,
                    child.downcast(),
                    app_state,
                )
            }
            Some(RESPONSIVE_LAYOUT_NARROW_VIEW_ID) => {
                let mut child = ResponsiveLayoutWidget::narrow_mut(&mut element);
                self.narrow.message(
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
