# Plan pushdown with Query Engine planners

## Purpose

Pushdown is a coordinated logical-plan transformation. A Query Engine adapter
changes the engine's native plan, while a connector accepts and executes a
Substrait plan for remote work. The SDK keeps those two transformations on one
contract without taking ownership of either planner.

After reading this document, a developer should be able to trace one operator
from a native planner callback, through connector planning, back into the
rewritten native tree, and finally into execution.

## The transformation

A pushdown attempt starts with two pieces of state:

- a native operator pattern owned by the Query Engine; and
- the remote Substrait plan already stored in the connector scan, if planning
  has previously pushed work into that scan.

The adapter proposes the complete remote plan it wants after the transformation.
The proposal is a candidate, not a patch for the connector to apply.

~~~text
native operator over connector scan
+ current accepted remote plan
        |
        | adapter recognizes one replaceable pattern
        | and constructs the complete next Substrait plan
        v
Substrait candidate + adapter planning context
        |
        | SDK validates the common contract
        | connector evaluates Data Source-specific semantics
        v
Exact(planned) | Retained(planned) | Unsupported
        |
        | adapter alone rewrites the native tree
        v
native residual tree over connector scan
+ accepted remote plan
~~~

The connector never edits a Spark, Trino, DataFusion, or DuckDB plan. The
adapter never decides Data Source-specific semantics from an operator name alone.
The SDK never converts Substrait expressions into a second SDK-owned
`ColumnRef`, `Condition`, or operator tree.

## Responsibilities

| Participant | Owns | Does not own |
| --- | --- | --- |
| Query Engine planner | Native logical-plan validity, optimizer scheduling, and engine semantics | Data Source-specific support or the remote plan |
| Query Engine adapter | Native pattern recognition, construction of the complete Substrait candidate, residual operators, and native plan replacement | Data Source-specific semantic guarantees |
| Connector SDK | Common Substrait validation, operator dispatch, outcome rules, serializable planned state, and execution-time contract checks | Native plan nodes, SQL dialects, or residual evaluation |
| Connector | Current Data Source/session facts, Data Source-specific semantic checks, translation of an accepted plan, and its execution | Native plan rewriting or Query Engine expressions |
| Data Source | Execution of the query or request produced by the Connector | The pushdown protocol |

These responsibilities create two interfaces:

~~~text
Query Engine adapter
    |
    | planning attempt / outcome and planned state
    v
Connector API and SDK
    |
    | validated candidate / source-aware decision
    v
Connector planner and executor
~~~

The upper interface is stable across connector implementations. The lower
interface is operator-specific so each connector can apply source knowledge at
the point where a planning decision is made.

## State and ownership

The following state crosses or supports the planning boundary. The names are
conceptual; language bindings may represent them differently.

| State | Created by | Meaning |
| --- | --- | --- |
| Native pattern | Query Engine planner or adapter | The operator or composite pattern the engine can remove or retain safely |
| Table reference | Adapter or metadata caller | Unresolved non-empty name path. The current SDK scaffold carries it in candidates and planned subplans; it is not proof of registration or Data Source ownership. |
| Current planned subplan | A prior successful attempt | The complete remote work already stored in the connector scan |
| Planning snapshot | Data Source Session | Session-local source facts hydrated before native optimizer callbacks; retained only by coordinator/driver planning state and never serialized to execution workers |
| Candidate plan | Adapter | The complete Substrait plan desired after this attempt, including the current remote work |
| Residual policy | Adapter | Whether this native pattern may remain above the scan |
| Adapter context | Adapter | The complete engine/session facts and semantic requirements for one complete remote plan; stored with that planned state so a later attempt and execution can present it again |
| Connector facts | Data Source Session | A session-local snapshot of Data Source product, version, type attributes, collation, time zone, snapshot, security identity, and other source facts needed by the candidate |
| Validated candidate | SDK | The candidate after common structural, type, extension, schema, and resource-bound checks |
| Planning context | SDK and Data Source Session | The effective facts used for the decision and its validation manifest |
| Planned subplan | SDK | Serializable accepted Substrait plan, output schema, table references, and non-secret validation manifest with an aggregate fingerprint. Binding resolution is not yet implemented. |
| Execution context | Adapter and caller | The adapter context used at planning plus deadline and cancellation needed to verify and run the planned subplan |

Credentials, connections, native plan nodes, and process-local pointers never
become part of a candidate or planned subplan. A connector obtains connector
facts from its live source/session boundary; an adapter does not assert facts
such as the source's current collation on the connector's behalf.
The current SDK validates that a table reference is non-empty, but does not yet
resolve it to a registered table binding or prove its Data Source ownership.
Callers must not treat a `TableReference` as that proof.

The adapter keeps two carriers distinct. Mutable planning state contains the
session-local snapshot identity, current planned subplan, complete adapter
context, and native callback bookkeeping. Once a scan is final, its serializable
execution payload contains only the planned subplan and complete adapter
context. Execution workers create or use their own Data Source Session and run
first-pull preflight; they never receive the planning snapshot identity.

## Planning interfaces

The adapter-facing planning interface has one request and three semantic
outcomes:

~~~text
PlanningAttempt {
    rule: Read | Projection | Filter | Limit | Aggregation | Sort | TopN | Join
    current_state: optional {
        planned_subplan: PlannedSubplan
        adapter_context: AdapterPlanningContext
    }
    candidate_plan: Substrait Plan
    residual_policy: ExactOnly | MayRetainOperator
    candidate_adapter_context: AdapterPlanningContext
}

PlanningResult =
    Exact   { planned_subplan }
  | Retained { planned_subplan }
  | Unsupported { reason }

plan_pushdown(session, PlanningSnapshot, PlanningAttempt)
    -> PlanningResult | PlanningError
~~~

`Unsupported` is a normal optimizer result. It leaves both the native plan and
the current planned subplan unchanged. `Exact` replaces the scan's planned
state and permits removal of the matching native pattern. `Retained` replaces
the scan's planned state but requires that pattern to remain. `PlanningError`
is reserved for an invalid request, a violated contract, or an operational
failure while planning.

Before creating the base connector scan, the adapter asks the Data Source Session
to hydrate a planning snapshot for the registered table bindings and expected
schemas. That preparation is a timeout-bound, cancellable source operation and
finishes before native optimizer callbacks begin. The common planning call then
performs no source I/O: it reads the named snapshot and returns synchronously to
the native planner. If a fact required for an exact decision is unavailable,
the connector returns `Unsupported` rather than blocking a callback or
guessing. A new binding or an invalidated snapshot requires preparation again.

Inside the library, the SDK coordinates common validation and the connector's
operator-specific rule:

~~~text
plan_pushdown(session, snapshot, attempt):
    if attempt.current_state is present:
        previous = restore(attempt.current_state.planned_subplan)
        verify_previous_identity(
            previous, attempt.current_state.adapter_context, snapshot)
    else:
        previous = none
    validated = validate_common_substrait_contract(attempt.candidate_plan)
    transition = validate_rule_transition(previous, validated, attempt.rule)
    connector_facts = snapshot.facts_for(validated)
    context = bind(attempt.candidate_adapter_context, connector_facts)

    decision = dispatch(attempt.rule, session.connector.planner, validated, context)

    if decision == Unsupported:
        return Unsupported

    if decision == Retained and attempt.residual_policy == ExactOnly:
        return PlanningError(residual_required)

    if decision == Exact and transition.introduces_residual_obligation:
        return PlanningError(residual_required)

    identity = validation_identity(validated, context, connector)
    manifest = validation_manifest(identity)
    planned = package(validated, manifest)
    return decision with planned
~~~

The connector-facing planner has one method per native planner pattern:

~~~text
ConnectorPlanner:
    plan_read(validated, context)        -> ConnectorDecision
    push_projection(validated, context)  -> ConnectorDecision
    push_filter(validated, context)      -> ConnectorDecision
    push_limit(validated, context)       -> ConnectorDecision
    push_aggregation(validated, context) -> ConnectorDecision
    push_sort(validated, context)        -> ConnectorDecision
    push_top_n(validated, context)       -> ConnectorDecision
    push_join(validated, context)        -> ConnectorDecision

ConnectorDecision = Exact | Retained | Unsupported
~~~

The connector evaluates the complete candidate. The SDK compares it with the
explicit current state to validate the operator-specific transition and to
determine whether this attempt introduces a new residual obligation.
An obligation already represented in the current plan belongs to an earlier
native residual and does not prevent an exact outcome for the current pattern.
The SDK validates the old fingerprint with the old adapter context. The
candidate adapter context is a complete normalized context for the complete
candidate, not a delta for the current pattern: the adapter carries forward all
still-relevant requirements, adds the current pattern's requirements, and drops
requirements only when their remote operation is removed. The candidate must
preserve the current plan except for the change allowed by the named rule;
otherwise the attempt is a `PlanningError`.
The connector does not return a residual expression tree or an arbitrary
accepted fragment. If an engine can negotiate a smaller unit, its adapter makes
another attempt with that smaller complete candidate.

## Adapter context

`AdapterPlanningContext` is a versioned shared-contract value, not an open map
of engine-specific properties. The initial form is:

~~~text
AdapterPlanningContextV1 {
    execution_domain: non-secret canonical domain identity
    semantic_requirements: {
        null_logic
        text_equality_ordering_and_trailing_spaces
        float_nan_ordering_and_signed_zero
        decimal_rounding_and_overflow
        temporal_interpretation
        expression_error_behavior
    }
    session_semantics: {
        time_zone
        locale
        other fields defined by this contract version
    }
    extension_requirements: sorted list of declared contract extensions
}
~~~

Each semantic-requirement field is a closed contract enum. It is required when
the candidate uses the affected operation and otherwise carries the explicit
`not_applicable` value; there are no implicit engine-specific defaults. The
execution domain is an SDK-issued canonical identity, not an engine object.
Adapters translate native semantics to this source-independent vocabulary; they
never put `spark.*`, `trino.*`, or another engine's objects in the context. A
connector interprets only this common vocabulary.

Canonical encoding materializes contract defaults, distinguishes absent from
present-empty where the schema permits both, sorts extension entries by URI and
name, uses the contract's numeric enum values, and length-delimits every field.
An unknown context version, enum value, field, or extension is
`invalid request`, not silently ignored. The connector contract owns the
concrete field and enum definitions.

## Validation identity

An accepted decision is valid only for the semantic inputs that produced it.
The SDK therefore fingerprints a versioned validation identity, not merely the
Substrait bytes or the current Data Source version:

~~~text
ValidationIdentity {
    connector_contract_version
    substrait_version
    connector_implementation_identity
    connector_semantic_rules_revision
    candidate_plan_digest
    analytics_output_schema_and_table_references
    normalized_adapter_context
    connector_fact_snapshot
}

ValidationManifest {
    one digest for each ValidationIdentity component
    aggregate fingerprint over the ordered component digests
}
~~~

`PlannedSubplan` stores the non-secret manifest, not the source values. This
allows execution to identify which components changed without exposing facts,
literals, or credentials. The representation is deterministic, versioned, and
length-delimited. It preserves distinctions that can change meaning, including
an absent fact versus a present empty value. A change to any listed input
invalidates the accepted state; identical inputs produce the same fingerprint. The connector
implementation identity and semantic-rules revision ensure that upgrading a
connector cannot silently reuse a decision made under different translation or
comparison rules.

Deadline, cancellation state, credentials, connections, and process-local
handles are execution concerns and are not fingerprint inputs. Credentials and
security identity are different: credentials remain outside the identity, while
the non-secret security identity belongs in connector facts when it can change
visible rows or expression semantics.

## Native-plan rewrite

The adapter performs a transaction-like transformation: it constructs and tests
a candidate before changing either plan.

~~~text
try_pushdown(native_pattern, connector_scan):
    previous = connector_scan.planned_state
    candidate = build_complete_candidate(native_pattern, previous.plan)
    candidate_context = build_complete_adapter_context(
        native_pattern, previous.adapter_context, candidate)

    result = plan_pushdown(
        connector_scan.session,
        connector_scan.planning_snapshot,
        rule_for(native_pattern),
        current_state=previous,
        candidate,
        residual_policy_for(native_pattern),
        candidate_context
    )

    match result:
        Exact(planned):
            new_scan = connector_scan.with(planned, candidate_context)
            return remove_native_pattern(native_pattern, new_scan)

        Retained(planned):
            new_scan = connector_scan.with(planned, candidate_context)
            return retain_native_pattern(native_pattern, new_scan)

        Unsupported:
            return native_pattern unchanged
~~~

Only `Exact` permits removal of the corresponding native pattern.
`Retained` changes the remote plan but preserves the native operator as the
authority for final results. An error aborts planning; it is not converted to
`Unsupported`.

## Initial operator rules

The initial contract supports read, projection, and filter. Each attempt still
contains a complete single-source `ReadRel` plan.

For these rules, `validate_rule_transition` uses structural equality rather
than semantic equivalence. The adapter preserves unchanged protobuf fields and
may make only the following change:

| Rule | Required current state | Allowed candidate change |
| --- | --- | --- |
| Read | Absent | Introduce one `ReadRel` with the registered table binding and base schema; filter, best-effort filter, and projection are absent |
| Projection | Present | Preserve the binding, base schema, filters, extensions, and all other fields; replace only the selection mask and matching `RelRoot` output names with a narrowing of the columns the current remote plan returns |
| Filter with `ExactOnly` | Present | Preserve every existing field, append exactly the offered predicate to `ReadRel.filter`, and append only extension declarations referenced by that predicate |
| Filter with `MayRetainOperator` | Present | Preserve every existing field, append exactly the offered predicate to `ReadRel.best_effort_filter`, and append only extension declarations referenced by that predicate |

Appending a filter means `new = and(old, offered)` when the field already
exists, with `old` as the left child and `offered` as the right child. It is a
structural construction rule, not CNF/DNF normalization or semantic
deduplication. The adapter avoids re-offering a native predicate by its engine's
own normalized identity. Limit, aggregation, sort, TopN, and join remain
`Unsupported` until this table has an allowed transition for them.

### Read

A read attempt establishes the first planned subplan for a connector scan. It is
exact-only: there is no earlier connector scan that can serve as a residual for
an unsupported base read.

~~~text
native table reference
    -> candidate ReadRel
    -> Exact(planned read)
    -> ConnectorScan(planned read)
~~~

### Projection

The adapter builds a candidate whose `ReadRel.projection` produces every
column required by the native plan that will remain. It may remove a native
projection only when the result is `Exact`.

~~~text
Project[a, c]                         ConnectorScan
  ConnectorScan(plan0)     Exact      planned = with_projection(plan0, [a, c])

Project[f(a)]                         Project[f(a)]
  ConnectorScan(plan0)     Retained     ConnectorScan
                                         planned = with_projection(plan0, [a])
~~~

A pruning-only engine hook may offer only column selection. An engine with
expression projection may offer a larger expression pattern. That difference
changes candidate construction, not the shared outcome rules.

### Filter

Filter negotiation has two distinct attempts because the Substrait field fixes
whether the predicate is exact before the connector returns a decision. The
adapter first offers `ReadRel.filter` with `ExactOnly`. If that is unsupported
and the native filter can remain, it may offer a second candidate using
`ReadRel.best_effort_filter` with `MayRetainOperator`.

~~~text
plan_filter(predicate, current_plan, can_retain):
    exact_candidate = with_filter(current_plan, predicate)
    exact = plan_pushdown(Filter, exact_candidate, ExactOnly)

    if exact is Exact:
        return Exact
    if exact is PlanningError:
        return error
    if not can_retain:
        return Unsupported

    hint_candidate = with_best_effort_filter(current_plan, predicate)
    hint = plan_pushdown(Filter, hint_candidate, MayRetainOperator)

    if hint is Retained:
        return Retained
    if hint is PlanningError:
        return error
    return Unsupported
~~~

A connector returns only `Exact` or `Unsupported` for an exact-filter
candidate, and only `Retained` or `Unsupported` for a best-effort candidate.
The SDK rejects a decision that contradicts the candidate field.

~~~text
Filter[p]
  ConnectorScan(plan0)

Exact:                         Retained:
ConnectorScan                  Filter[p]
  planned = with_filter(         ConnectorScan
      plan0, p)                    planned = with_best_effort_filter(
                                                plan0, p)

Unsupported:
Filter[p]
  ConnectorScan(plan0)
~~~

A candidate may contain nested expressions, disjunctions, or conjunctions.
The shared contract requires no CNF, DNF, or AND-only normal form. An adapter
may nevertheless offer individual predicates when its native planner contract
negotiates them individually.

## Accumulating operators

Each successful attempt replaces the scan's planned subplan. The next attempt
starts from that accepted plan and proposes another complete plan.

~~~text
start:
Project[a, b]
  Filter[p]
    Table(t)

after read:
Project[a, b]
  Filter[p]
    ConnectorScan(Read(t))

after exact projection:
Filter[p]
  ConnectorScan(ReadRel(table=t, projection=[a, b]))

after retained filter:
Filter[p]
  ConnectorScan(ReadRel(
      table=t,
      projection=[a, b],
      best_effort_filter=p))
~~~

The actual callback order is controlled by the Query Engine. The adapter must
therefore construct a valid complete candidate from the scan state it receives,
not assume that every engine visits operators in one common order.

## Execution interface

Planning produces serializable state. Execution consumes that state, its stored
complete adapter context, and execution controls; it does not consume a native
plan or an unaccepted candidate.

~~~text
execute(session, serialized_planned_subplan, execution_context):
    planned = restore_and_validate(serialized_planned_subplan)
    current_facts, guard = session.connector.preflight(planned)
    current_context = bind(execution_context.adapter_context, current_facts)
    current_identity = validation_identity(planned, current_context, connector)
    verify_manifest(planned, current_identity)
    source_request = session.connector.translate(planned, current_facts)
    stream = session.connector.execute(source_request, guard)
    return enforce_schema_bounds_provenance(stream, planned)
~~~

The adapter stores its sanitized planning context beside the serialized planned
subplan in the native scan or table handle and presents it again at execution.
`start_read` creates the operation without contacting the source. On the first
pull, the connector performs any metadata preflight needed to obtain current
facts before it issues the data query. The SDK then recomputes the full
validation identity, which includes the effective context, contract and
Substrait versions, connector identity and rules revision, plan, schema, and
bindings.
A connector may omit the preflight only when a session or snapshot guarantee
keeps every decision-relevant fact stable through execution.

Preflight establishes an execution guard, not just a point-in-time check. The
guard may be a source transaction or snapshot, immutable session settings, a
metadata version token validated in the same source operation, or explicit
semantics encoded in the generated request. It remains effective until the
source has evaluated every accepted operator whose meaning depends on it. If a
connector cannot establish such a guard for a semantic fact, it cannot return
`Exact` for a candidate that depends on that fact. It may use `Retained` only
when the remote operation remains conservative under every permitted change—for
example, a best-effort filter must still return a superset that the native
residual can reduce to the correct result.

A mismatch makes the first pull fail with `invalid state` and a structured
plan-invalidated reason, before the data query is issued. The current query does
not jump back into an optimizer callback from execution. Its caller may retry
the query, causing the Query Engine to plan again; a schema or object change
that also invalidates Physical Catalog registration remains
`stale registration`.

The connector executor must implement the accepted plan exactly. The SDK checks
the output schema, cancellation and deadline behavior, batch bounds, and
provenance. It does not evaluate residual predicates over Arrow batches.

## Query Engine mapping

The common interface does not require every engine to expose every rule. An
adapter maps only the native hooks whose rewrite and residual behavior it can
preserve.

| Query Engine | Stored planned state | Native rewrite owner |
| --- | --- | --- |
| Spark | Scan builder and resulting scan | Spark data-source pushdown callbacks |
| Trino | Connector table handle | Trino optimizer callbacks |
| DataFusion | Table provider scan state or adapter-owned execution node | DataFusion provider or custom optimizer rule |
| DuckDB | Table-function bind state or adapter-owned logical operator | DuckDB table-function callback or optimizer extension |

[Query-engine pushdown integration](query-engine-pushdown.md) shows how each
engine's callback inputs and return values map to the planning attempt and its
three outcomes.

## Future operators

Limit, aggregation, sort, TopN, and join use the same planning interface but
remain `Unsupported` until all four pieces exist:

1. common validation for the operator's complete candidate;
2. a connector planning method and exact execution semantics;
3. an adapter mapping for the target Query Engine's native planner contract;
   and
4. a native rewrite that preserves required residual or final operators.

The candidate granularity follows the native planner contract. For example,
TopN remains distinct from independently pushing sort and limit because Spark
and Trino negotiate the combined pattern atomically. A join candidate must
represent both inputs and the join as one source execution domain when the
engine replaces that whole pattern.

## Invariants

Changes to the planning API, connector rules, or adapters preserve these
properties:

- only the adapter rewrites a Query Engine's native plan;
- the adapter proposes a complete next remote plan rather than a patch;
- the connector decides one complete candidate with current source/session
  facts;
- only `Exact` removes the corresponding native pattern;
- `Retained` is valid only when the native pattern can remain;
- `Unsupported` changes neither native nor remote state;
- Substrait is the only shared relation and expression AST;
- persisted plans are serializable, revalidatable, and free of live resources
  and credentials; and
- a new operator requires validation, connector planning, execution, and an
  adapter rewrite before it can report acceptance.

The rationale for using Substrait instead of an SDK-owned expression and
operator tree is recorded in
[Use Substrait as the shared pushdown AST](design/use-substrait-as-pushdown-ast.md).
