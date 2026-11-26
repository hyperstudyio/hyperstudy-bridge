<script>
  // Inside the canvas: refits the view whenever `nonce` changes (a recipe or saved pipeline opened, Tidy pressed).
  // Waits a frame so that Svelte Flow has measured the new nodes first.
  import { useSvelteFlow } from '@xyflow/svelte';

  let { nonce = 0 } = $props();
  const { fitView } = useSvelteFlow();

  $effect(() => {
    if (!nonce) return;
    const frame = globalThis.requestAnimationFrame(() =>
      globalThis.requestAnimationFrame(() => fitView({ maxZoom: 1, padding: 0.25, duration: 200 }))
    );
    return () => globalThis.cancelAnimationFrame(frame);
  });
</script>
