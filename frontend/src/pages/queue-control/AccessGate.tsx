import { Show } from 'solid-js'
import { CircleUserRound, LoaderCircle, LockKeyhole, RadioTower } from 'lucide-solid'

interface AccessGateProps {
  gate: () => { kind: string; station: string; title: string; message: string }
}

/** Explains why the cockpit is unavailable: checking, signed out, read-only, or not an admin. */
export function AccessGate(props: AccessGateProps) {
  return (
    <section
      class="queue-control-gate"
      classList={{ 'is-checking': props.gate().kind === 'checking' }}
      role="status"
      aria-live="polite"
    >
      <span class="queue-control-gate-icon" aria-hidden="true">
        <Show when={props.gate().kind === 'checking'}>
          <LoaderCircle size={20} strokeWidth={1.8} />
        </Show>
        <Show when={props.gate().kind === 'signed-out'}>
          <CircleUserRound size={20} strokeWidth={1.8} />
        </Show>
        <Show when={props.gate().kind === 'read-only'}>
          <RadioTower size={20} strokeWidth={1.8} />
        </Show>
        <Show when={props.gate().kind === 'not-admin'}>
          <LockKeyhole size={20} strokeWidth={1.8} />
        </Show>
      </span>
      <div class="queue-control-gate-copy">
        <p class="eyebrow">{props.gate().station}</p>
        <h1>{props.gate().title}</h1>
        <p>{props.gate().message}</p>
        <Show when={props.gate().kind === 'signed-out'}>
          <a class="queue-control-gate-action" href="/auth">sign in</a>
        </Show>
      </div>
    </section>
  )
}
