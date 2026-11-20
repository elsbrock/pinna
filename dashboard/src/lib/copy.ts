// Copy text. On plain http (the satellite on the LAN) the Clipboard API is
// missing, so fall back to a selected textarea.
export async function copyText(text: string): Promise<void> {
  try {
    await navigator.clipboard.writeText(text);
  } catch {
    // Selecting the textarea takes the focus: give it back afterwards, so the
    // keyboard stays on the button that copied.
    const focused = document.activeElement;
    const area = document.createElement("textarea");
    area.value = text;
    area.setAttribute("readonly", "");
    area.style.position = "fixed";
    area.style.opacity = "0";
    document.body.append(area);
    area.select();
    document.execCommand("copy");
    area.remove();
    if (focused instanceof HTMLElement) focused.focus();
  }
}
