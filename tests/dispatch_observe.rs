//! End-to-end: drive `rig_compose::dispatch_tool_invocations_with_hooks`
//! with [`DispatchObserveHook`] and assert the kernel-direct dispatch path
//! emits the same `tool.invoked` / `tool.completed` / `tool.skipped` / `tool.terminated`
//! event shape as the agent `PromptHook` path.

#![cfg(all(feature = "subscriber", feature = "compose"))]
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::expect_used
)]

use std::sync::Arc;

use async_trait::async_trait;
use rig_compose::{
    Agent, GenericAgent, InvestigationContext, KernelError, LocalTool, Skill, SkillOutcome,
    SkillRegistry, ToolDispatchAction, ToolDispatchHook, ToolInvocation, ToolRegistry, ToolSchema,
    dispatch_tool_invocations_with_hooks,
};
use rig_tap::{CapturingLayer, DispatchObserveHook, EventKind};
use serde_json::json;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

#[tokio::test]
async fn observes_invoked_and_completed_for_continue_path() {
    let capture = CapturingLayer::new();
    let subscriber = tracing_subscriber::registry().with(capture.clone());
    let _guard = subscriber.set_default();

    let tools = registry_with_echo();
    let observe = DispatchObserveHook::new("conv-1");
    let invs = vec![ToolInvocation::new("echo", json!({ "msg": "hi" })).unwrap()];

    let results = dispatch_tool_invocations_with_hooks(&tools, &invs, &[&observe])
        .await
        .unwrap();
    assert_eq!(results.len(), 1);

    let events = capture.snapshot();
    assert_eq!(events.len(), 2, "expected invoked + completed");
    assert_eq!(events[0].conversation_id, "conv-1");

    let (invoked_call_id, invoked_tool) = match &events[0].kind {
        EventKind::ToolInvoked {
            tool_name, call_id, ..
        } => (call_id.clone(), tool_name.clone()),
        other => panic!("expected ToolInvoked, got {other:?}"),
    };
    assert_eq!(invoked_tool, "echo");

    match &events[1].kind {
        EventKind::ToolCompleted {
            tool_name, call_id, ..
        } => {
            assert_eq!(tool_name, "echo");
            assert_eq!(call_id, &invoked_call_id, "call_id must pair");
        }
        other => panic!("expected ToolCompleted, got {other:?}"),
    }
}

#[tokio::test]
async fn observes_terminated_when_gate_hook_terminates() {
    let capture = CapturingLayer::new();
    let subscriber = tracing_subscriber::registry().with(capture.clone());
    let _guard = subscriber.set_default();

    let tools = registry_with_echo();
    let observe = DispatchObserveHook::new("conv-2");
    let gate = AlwaysTerminate {
        reason: "budget exhausted".into(),
    };
    let invs = vec![ToolInvocation::new("echo", json!({ "msg": "no" })).unwrap()];

    let err = dispatch_tool_invocations_with_hooks(&tools, &invs, &[&observe, &gate])
        .await
        .unwrap_err();
    assert!(matches!(err, KernelError::ToolDispatchTerminated(_)));

    let events = capture.snapshot();
    assert_eq!(events.len(), 2, "expected invoked + terminated");

    let invoked_call_id = match &events[0].kind {
        EventKind::ToolInvoked { call_id, .. } => call_id.clone(),
        other => panic!("expected ToolInvoked, got {other:?}"),
    };
    match &events[1].kind {
        EventKind::ToolTerminated {
            tool_name,
            call_id,
            reason,
        } => {
            assert_eq!(tool_name, "echo");
            assert_eq!(call_id, &invoked_call_id, "call_id must pair");
            assert!(
                reason.contains("budget exhausted"),
                "reason should propagate, got: {reason}"
            );
        }
        other => panic!("expected ToolTerminated, got {other:?}"),
    }
}

#[tokio::test]
async fn observes_terminated_when_gate_runs_before_observer() {
    let capture = CapturingLayer::new();
    let subscriber = tracing_subscriber::registry().with(capture.clone());
    let _guard = subscriber.set_default();

    let tools = registry_with_echo();
    let gate = AlwaysTerminate {
        reason: "budget exhausted".into(),
    };
    let observe = DispatchObserveHook::new("conv-2b");
    let invs = vec![ToolInvocation::new("echo", json!({ "msg": "no" })).unwrap()];

    let err = dispatch_tool_invocations_with_hooks(&tools, &invs, &[&gate, &observe])
        .await
        .unwrap_err();
    assert!(matches!(err, KernelError::ToolDispatchTerminated(_)));

    let events = capture.snapshot();
    assert_eq!(events.len(), 2, "expected synthetic invoked + terminated");

    let invoked_call_id = match &events[0].kind {
        EventKind::ToolInvoked {
            tool_name, call_id, ..
        } => {
            assert_eq!(tool_name, "echo");
            call_id.clone()
        }
        other => panic!("expected ToolInvoked, got {other:?}"),
    };
    match &events[1].kind {
        EventKind::ToolTerminated {
            tool_name,
            call_id,
            reason,
        } => {
            assert_eq!(tool_name, "echo");
            assert_eq!(call_id, &invoked_call_id, "call_id must pair");
            assert!(reason.contains("budget exhausted"));
        }
        other => panic!("expected ToolTerminated, got {other:?}"),
    }
}

#[tokio::test]
async fn observes_terminated_when_tool_errors() {
    let capture = CapturingLayer::new();
    let subscriber = tracing_subscriber::registry().with(capture.clone());
    let _guard = subscriber.set_default();

    let tools = registry_with_boom();
    let observe = DispatchObserveHook::new("conv-3");
    let invs = vec![ToolInvocation::new("boom", json!({})).unwrap()];

    let err = dispatch_tool_invocations_with_hooks(&tools, &invs, &[&observe])
        .await
        .unwrap_err();
    drop(err);

    let events = capture.snapshot();
    assert_eq!(events.len(), 2, "expected invoked + terminated");
    let invoked_id = match &events[0].kind {
        EventKind::ToolInvoked { call_id, .. } => call_id.clone(),
        other => panic!("expected ToolInvoked, got {other:?}"),
    };
    match &events[1].kind {
        EventKind::ToolTerminated { call_id, .. } => {
            assert_eq!(call_id, &invoked_id, "call_id must pair");
        }
        other => panic!("expected ToolTerminated, got {other:?}"),
    }
}

#[tokio::test]
async fn observes_skipped_when_gate_runs_after_observer() {
    let capture = CapturingLayer::new();
    let subscriber = tracing_subscriber::registry().with(capture.clone());
    let _guard = subscriber.set_default();

    let tools = registry_with_echo();
    let observe = DispatchObserveHook::new("conv-4");
    let gate = AlwaysSkip {
        reason: "policy denied".into(),
    };
    let invs = vec![ToolInvocation::new("echo", json!({ "msg": "skip" })).unwrap()];

    let results = dispatch_tool_invocations_with_hooks(&tools, &invs, &[&observe, &gate])
        .await
        .unwrap();
    assert_eq!(results.len(), 1);

    let events = capture.snapshot();
    assert_eq!(events.len(), 2, "expected invoked + skipped");

    let invoked_call_id = match &events[0].kind {
        EventKind::ToolInvoked { call_id, .. } => call_id.clone(),
        other => panic!("expected ToolInvoked, got {other:?}"),
    };
    match &events[1].kind {
        EventKind::ToolSkipped {
            tool_name,
            call_id,
            reason,
        } => {
            assert_eq!(tool_name, "echo");
            assert_eq!(call_id, &invoked_call_id, "call_id must pair");
            assert_eq!(reason, "policy denied");
        }
        other => panic!("expected ToolSkipped, got {other:?}"),
    }
}

#[tokio::test]
async fn observes_skipped_when_gate_runs_before_observer() {
    let capture = CapturingLayer::new();
    let subscriber = tracing_subscriber::registry().with(capture.clone());
    let _guard = subscriber.set_default();

    let tools = registry_with_echo();
    let gate = AlwaysSkip {
        reason: "policy denied".into(),
    };
    let observe = DispatchObserveHook::new("conv-5");
    let invs = vec![ToolInvocation::new("echo", json!({ "msg": "skip" })).unwrap()];

    let results = dispatch_tool_invocations_with_hooks(&tools, &invs, &[&gate, &observe])
        .await
        .unwrap();
    assert_eq!(results.len(), 1);

    let events = capture.snapshot();
    assert_eq!(events.len(), 2, "expected synthetic invoked + skipped");

    let invoked_call_id = match &events[0].kind {
        EventKind::ToolInvoked {
            tool_name, call_id, ..
        } => {
            assert_eq!(tool_name, "echo");
            call_id.clone()
        }
        other => panic!("expected ToolInvoked, got {other:?}"),
    };
    match &events[1].kind {
        EventKind::ToolSkipped {
            tool_name,
            call_id,
            reason,
        } => {
            assert_eq!(tool_name, "echo");
            assert_eq!(call_id, &invoked_call_id, "call_id must pair");
            assert_eq!(reason, "policy denied");
        }
        other => panic!("expected ToolSkipped, got {other:?}"),
    }
}

#[tokio::test]
async fn observes_generic_agent_lifecycle_events() {
    let capture = CapturingLayer::new();
    let subscriber = tracing_subscriber::registry().with(capture.clone());
    let _guard = subscriber.set_default();

    let skills = SkillRegistry::new();
    skills.register(Arc::new(DeltaSkill));
    let tools = ToolRegistry::new();
    let observe = DispatchObserveHook::new("conv-agent").into_arc();
    let agent = GenericAgent::builder("agent")
        .with_skills(["skill.delta"])
        .with_lifecycle_hook(observe.clone())
        .build(&skills, &tools)
        .unwrap();
    let mut ctx = InvestigationContext::new("entity", "partition").with_signal("run");

    let result = agent.step(&mut ctx).await.unwrap();
    assert_eq!(result.skills_run, vec!["skill.delta"]);
    assert_eq!(result.confidence, 0.25);

    // Drop the agent (and our local Arc) so `DispatchObserveHook::drop`
    // fires and emits the terminal `compose.kernel_shutdown`.
    drop(agent);
    drop(observe);

    let events = capture.snapshot();
    assert_eq!(events.len(), 4);
    assert!(
        events
            .iter()
            .all(|event| event.conversation_id == "conv-agent")
    );

    let kernel_id = match &events[0].kind {
        EventKind::ComposeKernelStart {
            kernel_id,
            skills_registered,
            tools_registered,
        } => {
            assert_eq!(skills_registered, &Some(1));
            assert_eq!(tools_registered, &Some(0));
            kernel_id.clone()
        }
        other => panic!("expected ComposeKernelStart, got {other:?}"),
    };
    match &events[1].kind {
        EventKind::ComposeLoopIteration {
            kernel_id: loop_kernel,
            iteration,
            skill_id,
            confidence,
        } => {
            assert_eq!(loop_kernel, &kernel_id);
            assert_eq!(iteration, &0);
            // Loop iteration is per-step now, not per-skill.
            assert!(skill_id.is_none());
            assert!(confidence.is_none());
        }
        other => panic!("expected ComposeLoopIteration, got {other:?}"),
    }
    match &events[2].kind {
        EventKind::ComposeSkillResolved {
            kernel_id: skill_kernel,
            skill_id,
            applies,
            delta,
            confidence,
        } => {
            assert_eq!(skill_kernel, &kernel_id);
            assert_eq!(skill_id, "skill.delta");
            assert!(*applies);
            assert_eq!(delta, &Some(0.25));
            assert_eq!(confidence, &Some(0.25));
        }
        other => panic!("expected ComposeSkillResolved, got {other:?}"),
    }
    match &events[3].kind {
        EventKind::ComposeKernelShutdown {
            kernel_id: shutdown_kernel,
            reason,
        } => {
            assert_eq!(shutdown_kernel, &kernel_id);
            assert_eq!(reason, "normal");
        }
        other => panic!("expected ComposeKernelShutdown, got {other:?}"),
    }
}

fn registry_with_echo() -> ToolRegistry {
    let tools = ToolRegistry::new();
    let schema = ToolSchema {
        name: "echo".into(),
        description: "echo the input".into(),
        args_schema: json!({ "type": "object" }),
        result_schema: json!({ "type": "object" }),
    };
    let tool = LocalTool::new(schema, |v| async move { Ok(v) });
    tools.register(Arc::new(tool));
    tools
}

fn registry_with_boom() -> ToolRegistry {
    let tools = ToolRegistry::new();
    let schema = ToolSchema {
        name: "boom".into(),
        description: "always fails".into(),
        args_schema: json!({ "type": "object" }),
        result_schema: json!({ "type": "object" }),
    };
    let tool = LocalTool::new(schema, |_| async {
        Err(KernelError::ToolFailed("intentional".into()))
    });
    tools.register(Arc::new(tool));
    tools
}

struct AlwaysTerminate {
    reason: String,
}

#[async_trait]
impl ToolDispatchHook for AlwaysTerminate {
    async fn before_invocation(
        &self,
        _invocation: &ToolInvocation,
    ) -> Result<ToolDispatchAction, KernelError> {
        Ok(ToolDispatchAction::Terminate {
            reason: self.reason.clone(),
        })
    }
}

struct AlwaysSkip {
    reason: String,
}

#[async_trait]
impl ToolDispatchHook for AlwaysSkip {
    async fn before_invocation(
        &self,
        _invocation: &ToolInvocation,
    ) -> Result<ToolDispatchAction, KernelError> {
        Ok(ToolDispatchAction::Skip {
            output: json!({ "status": "skipped" }),
            reason: Some(self.reason.clone()),
        })
    }
}

struct DeltaSkill;

#[async_trait]
impl Skill for DeltaSkill {
    fn id(&self) -> &str {
        "skill.delta"
    }

    fn applies(&self, ctx: &InvestigationContext) -> bool {
        ctx.has_signal("run")
    }

    async fn execute(
        &self,
        _ctx: &mut InvestigationContext,
        _tools: &ToolRegistry,
    ) -> Result<SkillOutcome, KernelError> {
        Ok(SkillOutcome::noop().with_delta(0.25))
    }
}
