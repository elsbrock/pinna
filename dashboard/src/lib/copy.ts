// Copy text. On plain http (the satellite on the LAN) the Clipboard API is
// missing, so fall back to a selected textarea. Says whether it copied: a
// caller that hands the text on (Open an issue) must not claim it did.
export async function copyText(text: string): Promise<boolean> {
  try {
    await navigator.clipboard.writeText(text);
    return true;
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
    let copied = false;
    try {
      copied = document.execCommand("copy");
    } catch {
      // Refused: copied stays false.
    }
    area.remove();
    if (focused instanceof HTMLElement) focused.focus();
    return copied;
  }
}
