import { useEffect } from 'preact/hooks'

export const Nip46SignerDeepLink = () => {
  useEffect(() => {
    if (typeof document === 'undefined') return

    let injected: HTMLAnchorElement | null = null
    let innerObserver: MutationObserver | null = null

    const removeInjected = () => {
      if (injected?.isConnected) injected.remove()
      injected = null
    }

    const sync = () => {
      const qrWrap = document.querySelector<HTMLElement>('.nui-qr-wrap')
      const uri = qrWrap?.querySelector<HTMLElement>('.nui-key-display')?.textContent?.trim()

      if (!qrWrap || !uri || !uri.startsWith('nostrconnect://')) {
        removeInjected()
        return
      }
      if (injected?.isConnected && injected.getAttribute('href') === uri) return

      removeInjected()
      const link = document.createElement('a')
      link.href = uri
      link.className = 'nui-open-signer'
      link.rel = 'noopener noreferrer'

      const icon = document.createElement('span')
      icon.setAttribute('aria-hidden', 'true')
      icon.textContent = '->'

      const label = document.createElement('span')
      label.textContent = 'Open in signer app'

      link.append(icon, label)
      qrWrap.insertBefore(link, qrWrap.firstChild)
      injected = link
    }

    const attachInner = (root: Element) => {
      innerObserver?.disconnect()
      innerObserver = new MutationObserver(sync)
      innerObserver.observe(root, { childList: true, subtree: true, characterData: true })
      sync()
    }

    let outerInterval: ReturnType<typeof setInterval> | null = setInterval(() => {
      const root = document.querySelector('.nui-widget') ?? document.querySelector('.nui-modal-overlay')
      if (!root) return
      if (outerInterval) {
        clearInterval(outerInterval)
        outerInterval = null
      }
      attachInner(root)
    }, 100)

    const existing = document.querySelector('.nui-widget') ?? document.querySelector('.nui-modal-overlay')
    if (existing) {
      if (outerInterval) {
        clearInterval(outerInterval)
        outerInterval = null
      }
      attachInner(existing)
    }

    return () => {
      if (outerInterval) clearInterval(outerInterval)
      innerObserver?.disconnect()
      removeInjected()
    }
  }, [])

  return null
}
