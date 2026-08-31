(() => {
  if (typeof globalThis.TextEncoder === 'undefined') {
    globalThis.TextEncoder = class TextEncoder {
      get encoding() { return 'utf-8' }

      encode(input = '') {
        const bytes = []
        for (const character of String(input)) {
          const codePoint = character.codePointAt(0)
          if (codePoint <= 0x7f) bytes.push(codePoint)
          else if (codePoint <= 0x7ff) {
            bytes.push(0xc0 | (codePoint >> 6), 0x80 | (codePoint & 0x3f))
          } else if (codePoint <= 0xffff) {
            bytes.push(
              0xe0 | (codePoint >> 12),
              0x80 | ((codePoint >> 6) & 0x3f),
              0x80 | (codePoint & 0x3f),
            )
          } else {
            bytes.push(
              0xf0 | (codePoint >> 18),
              0x80 | ((codePoint >> 12) & 0x3f),
              0x80 | ((codePoint >> 6) & 0x3f),
              0x80 | (codePoint & 0x3f),
            )
          }
        }
        return Uint8Array.from(bytes)
      }

      encodeInto(input, destination) {
        const encoded = this.encode(input)
        let written = Math.min(encoded.length, destination.length)
        while (written > 0 && written < encoded.length && (encoded[written] & 0xc0) === 0x80) written--
        destination.set(encoded.subarray(0, written))
        return {
          read: new TextDecoder().decode(encoded.subarray(0, written)).length,
          written,
        }
      }
    }
  }

  if (typeof globalThis.TextDecoder === 'undefined') {
    globalThis.TextDecoder = class TextDecoder {
      constructor(_label = 'utf-8', options = {}) {
        this.encoding = 'utf-8'
        this.fatal = options.fatal === true
        this.ignoreBOM = options.ignoreBOM === true
      }

      decode(input = new Uint8Array()) {
        const bytes = input instanceof Uint8Array
          ? input
          : new Uint8Array(input.buffer ?? input, input.byteOffset ?? 0, input.byteLength)
        let result = ''
        let index = 0
        while (index < bytes.length) {
          const first = bytes[index++]
          let codePoint
          let remaining
          if (first <= 0x7f) {
            codePoint = first
            remaining = 0
          } else if (first >= 0xc2 && first <= 0xdf) {
            codePoint = first & 0x1f
            remaining = 1
          } else if (first >= 0xe0 && first <= 0xef) {
            codePoint = first & 0x0f
            remaining = 2
          } else if (first >= 0xf0 && first <= 0xf4) {
            codePoint = first & 0x07
            remaining = 3
          } else {
            if (this.fatal) throw new TypeError('Invalid UTF-8 sequence')
            result += '\ufffd'
            continue
          }

          if (index + remaining > bytes.length) {
            if (this.fatal) throw new TypeError('Incomplete UTF-8 sequence')
            result += '\ufffd'
            break
          }
          let valid = true
          for (let offset = 0; offset < remaining; offset++) {
            const next = bytes[index + offset]
            if ((next & 0xc0) !== 0x80) {
              valid = false
              break
            }
            codePoint = (codePoint << 6) | (next & 0x3f)
          }
          if (
            !valid
            || (remaining === 2 && codePoint < 0x800)
            || (remaining === 3 && codePoint < 0x10000)
            || codePoint > 0x10ffff
            || (codePoint >= 0xd800 && codePoint <= 0xdfff)
          ) {
            if (this.fatal) throw new TypeError('Invalid UTF-8 sequence')
            result += '\ufffd'
            continue
          }
          index += remaining
          result += String.fromCodePoint(codePoint)
        }
        if (!this.ignoreBOM && result.charCodeAt(0) === 0xfeff) return result.slice(1)
        return result
      }
    }
  }
})()
