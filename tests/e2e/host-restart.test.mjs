import { sleep, t } from './harness.mjs';

const dialogTitle = `document.querySelector('.dialog h2')?.textContent || ''`;

export const area = {
  name: 'host-restart',
  needs: ['auth'],
  async run(h) {
    const a = h.a;
    const computer = `d.devices().find(x => x.id === ${JSON.stringify(h.hostId)})`;
    const openedOnce = await a.v(`(() => {
      const id = (${computer}).id;
      d.open(id);
      const first = d.session()?.info().id;
      d.open(id);
      return !!first && first === d.session()?.info().id;
    })()`);
    h.check('a second open while connecting keeps the page’s only session', openedOnce);
    const connected = `!!s && s.info().state === 'active' && s.info().input === 'open' && s.info().w > 0`;
    await a.until(connected, 'initial browser session');
    for (const delay of [4000, 24000]) {
      const previous = await a.v(`s.info().id`);
      await a.until(`(${computer})?.controller?.connecting === false`, 'the server sees the established holder');
      await a.eval(`{
        window.restartDialogs = [];
        let last;
        const observer = new MutationObserver(() => {
          const current = document.querySelector('.dialog h2');
          if (current && current !== last) restartDialogs.push(current.textContent);
          last = current;
        });
        observer.observe(document.body, { subtree: true, childList: true });
        window.stopRestartObserver = () => observer.disconnect();
      }`);
      h.stopTestHost();
      await a.until(`(${computer})?.online === false`, 'the killed host is offline');
      await sleep(delay);
      h.startTestHost();
      await a.until(`!!(${dialogTitle})`, 'the old session ends after the host starts');
      const title = await a.eval(dialogTitle);
      h.check(`B10 after ${delay / 1000}s: the computer restarted`, title === t('session.restartedTitle'), title);
      if (!await a.clickText('.dialog button', t('session.reconnect'), { exact: true })) throw new Error(`no reconnect button: ${title}`);
      await a.until(`(${connected}) || !!(${dialogTitle})`, 'one reconnect click connects or shows a question', 30000);
      await sleep(1500);
      const result = await a.v(`({ active: !!s && s.info().state === 'active', id: s?.info().id, dialogs: window.restartDialogs })`);
      await a.eval('window.stopRestartObserver()');
      h.check(`B10 after ${delay / 1000}s: one click connects without another dialog`, result.active && result.id !== previous && result.dialogs.length === 1, JSON.stringify(result));
      if (!result.active) break;
    }
    await a.v('s && s.leave()');
  },
};
