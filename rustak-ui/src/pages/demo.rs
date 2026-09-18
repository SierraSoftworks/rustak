//! The control gallery.
//!
//! Every shared component, in every state, on one page and with no server
//! behind it. It exists so that a change to a primitive can be reviewed against
//! all of its states at once rather than against whichever page happens to use
//! the one that broke.
//!
//! Debug builds only, alongside the fixtures it renders with.

use rustak_api::{AuditOutcome, ComponentStatus, MissionRoleKind};
use yew::prelude::*;

use crate::components::{
    Alert, AlertKind, Button, ButtonGroup, ButtonKind, Card, Center, EmptyState, Field, FileDrop,
    Layout, LoadingNote, NumberInput, RoleBadge, SecretInput, Select, SelectOption, Stat,
    StatusPill, StatusTone, Switch, TextArea, TextInput, XmlView,
};

#[function_component(DemoControls)]
pub fn demo_controls() -> Html {
    html! {
        <Layout>
            <main class="app-main">
                <div class="app-container gallery">
                    <h1 class="page-title__heading">{ "Controls" }</h1>
                    <p class="page-title__subtitle">
                        { "Every shared component, in every state. Debug builds only." }
                    </p>

                    <Buttons />
                    <Pills />
                    <Alerts />
                    <Inputs />
                    <Files />
                    <Documents />
                    <Furniture />
                </div>
            </main>
        </Layout>
    }
}

#[function_component(Buttons)]
fn buttons() -> Html {
    let noop = Callback::noop();
    html! {
        <Card title="Buttons">
            <div class="gallery__row">
                <Button onclick={noop.clone()}>{ "Default" }</Button>
                <Button kind={ButtonKind::Primary} onclick={noop.clone()}>{ "Primary" }</Button>
                <Button kind={ButtonKind::Danger} onclick={noop.clone()}>{ "Danger" }</Button>
                <Button kind={ButtonKind::Subtle} onclick={noop.clone()}>{ "Subtle" }</Button>
                <Button disabled=true onclick={noop.clone()}>{ "Disabled" }</Button>
                <Button busy=true onclick={noop.clone()}>{ "Busy" }</Button>
                <Button small=true onclick={noop.clone()}>{ "Small" }</Button>
                <Button large=true kind={ButtonKind::Primary} onclick={noop.clone()}>
                    { "Large" }
                </Button>
            </div>
            <div class="gallery__row">
                <ButtonGroup label="Grouped actions">
                    <Button small=true onclick={noop.clone()}>{ "Promote" }</Button>
                    <Button small=true kind={ButtonKind::Danger} onclick={noop}>
                        { "Suspend" }
                    </Button>
                </ButtonGroup>
            </div>
        </Card>
    }
}

#[function_component(Pills)]
fn pills() -> Html {
    html! {
        <Card title="Status pills">
            <div class="gallery__row">
                { for AuditOutcome::ALL.iter().map(|outcome| html! {
                    <StatusPill
                        tone={StatusTone::of_outcome(*outcome)}
                        label={outcome.label()}
                    />
                }) }
                { for ComponentStatus::ALL.iter().map(|status| html! {
                    <StatusPill
                        tone={StatusTone::of_component(*status)}
                        label={status.label()}
                        title="Shown on hover"
                    />
                }) }
            </div>
        </Card>
    }
}

#[function_component(Alerts)]
fn alerts() -> Html {
    html! {
        <Card title="Alerts">
            <Alert kind={AlertKind::Error} title="Something went wrong"
                message="With a longer explanation beneath it." />
            <Alert kind={AlertKind::Warning} title="Worth a look"
                message="Dismissible, and carrying an action."
                on_close={Callback::noop()}>
                <Button small=true onclick={Callback::noop()}>{ "Do the thing" }</Button>
            </Alert>
            <Alert kind={AlertKind::Info} title="For information" />
            <Alert kind={AlertKind::Success} title="That worked"
                message="Nothing further is needed." />
        </Card>
    }
}

#[function_component(Inputs)]
fn inputs() -> Html {
    let text = use_state(|| "avery".to_string());
    let area = use_state(|| "tak.example.com\n203.0.113.24".to_string());
    let number = use_state(|| Some(8446i64));
    let choice = use_state(|| Some(AttrValue::from("rsa2048")));
    let secret = use_state(|| "not-a-real-token".to_string());
    let switched = use_state(|| true);

    html! {
        <Card title="Form controls">
            <Field id="gallery-text" label="Text" required=true help="With help beneath it.">
                <TextInput
                    id="gallery-text"
                    value={(*text).clone()}
                    onchange={let text = text.clone(); Callback::from(move |v| text.set(v))}
                />
            </Field>

            <Field id="gallery-invalid" label="Text, refused"
                error="That username is already taken.">
                <TextInput
                    id="gallery-invalid"
                    value={(*text).clone()}
                    invalid=true
                    onchange={Callback::noop()}
                />
            </Field>

            <Field id="gallery-area" label="Text area">
                <TextArea
                    id="gallery-area"
                    value={(*area).clone()}
                    monospace=true
                    onchange={let area = area.clone(); Callback::from(move |v| area.set(v))}
                />
            </Field>

            <Field id="gallery-number" label="Number">
                <NumberInput
                    id="gallery-number"
                    value={*number}
                    onchange={let number = number.clone(); Callback::from(move |v| number.set(v))}
                />
            </Field>

            <Field id="gallery-select" label="Select">
                <Select
                    id="gallery-select"
                    value={(*choice).clone()}
                    options={vec![
                        SelectOption::new("rsa2048", "RSA 2048 (most compatible)"),
                        SelectOption::new("ecdsa_p256", "ECDSA P-256"),
                    ]}
                    onchange={Callback::from(move |v: Option<String>| {
                        choice.set(v.map(AttrValue::from))
                    })}
                />
            </Field>

            <Field id="gallery-secret" label="Secret" help="Masked until asked for.">
                <SecretInput
                    id="gallery-secret"
                    value={(*secret).clone()}
                    onchange={let secret = secret.clone(); Callback::from(move |v| secret.set(v))}
                />
            </Field>

            <Field id="gallery-switch" label="Switch">
                <Switch
                    id="gallery-switch"
                    checked={*switched}
                    label="Require a client certificate"
                    onchange={Callback::from(move |v| switched.set(v))}
                />
            </Field>

            <Field id="gallery-disabled" label="Disabled">
                <TextInput
                    id="gallery-disabled"
                    value="Cannot be edited"
                    disabled=true
                    onchange={Callback::noop()}
                />
            </Field>
        </Card>
    }
}

/// A CoT event, which is the document `XmlView` exists for.
///
/// Deliberately contains an escaped `&` and a comment: both are places where a
/// viewer that reached the DOM as markup, or one that dropped what it could
/// not colour, would show an operator something the client did not send.
const SAMPLE_COT: &str = concat!(
    "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
    "<event version=\"2.0\" uid=\"ANDROID-2f1c9a7b4e0d\" type=\"a-f-G-U-C\"\n",
    "       time=\"2026-09-18T12:00:00.000Z\" start=\"2026-09-18T12:00:00.000Z\"\n",
    "       stale=\"2026-09-18T12:02:00.000Z\" how=\"m-g\">\n",
    "  <point lat=\"51.50735\" lon=\"-0.12776\" hae=\"12.4\" ce=\"9.5\" le=\"9999999.0\"/>\n",
    "  <detail>\n",
    "    <contact callsign=\"QUINN &amp; CO\" endpoint=\"*:-1:stcp\"/>\n",
    "    <__group name=\"Cyan\" role=\"Team Member\"/>\n",
    "    <!-- written by a plugin -->\n",
    "    <takv device=\"Pixel 8\" platform=\"ATAK-CIV\" os=\"34\" version=\"5.2.0\"/>\n",
    "  </detail>\n",
    "</event>",
);

#[function_component(Files)]
fn files() -> Html {
    html! {
        <Card title="Choosing a file">
            <FileDrop
                id="gallery-file-drop"
                help="Also reachable from a keyboard: it is a label around a real file input."
                onfiles={Callback::noop()}
            />
            <FileDrop
                id="gallery-file-drop-busy"
                label="A zone that is already uploading"
                busy=true
                onfiles={Callback::noop()}
            />
        </Card>
    }
}

#[function_component(Documents)]
fn documents() -> Html {
    html! {
        <>
            <Card title="Mission roles">
                <div class="gallery__row">
                    { for MissionRoleKind::ALL.iter().map(|role| html! {
                        <RoleBadge key={role.as_str()} role={*role} />
                    }) }
                </div>
            </Card>

            <Card title="An XML document">
                <XmlView xml={SAMPLE_COT} label="A sample CoT event" />
            </Card>
        </>
    }
}

#[function_component(Furniture)]
fn furniture() -> Html {
    html! {
        <>
            <Card title="Figures">
                <div class="stat-row">
                    <Stat label="Accounts" value="4" />
                    <Stat label="Administrators" value="1" />
                    <Stat label="Suspended" value="1" detail="Cannot sign in or connect." />
                </div>
            </Card>

            <Card title="Waiting, and nothing to show">
                <LoadingNote />
                <EmptyState
                    title="Nothing here yet"
                    message="With a line saying what would put something here."
                >
                    <Button kind={ButtonKind::Primary} small=true onclick={Callback::noop()}>
                        { "Add one" }
                    </Button>
                </EmptyState>
            </Card>

            <Card title="Centred panel">
                <div class="gallery__centred">
                    <Center>
                        <div class="auth-card">
                            <h2 class="auth-card__title">{ "Sign in" }</h2>
                            <p class="auth-card__lead">{ "As the login screen renders it." }</p>
                        </div>
                    </Center>
                </div>
            </Card>
        </>
    }
}
