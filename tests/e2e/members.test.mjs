// The admin creates a member and copies the invitation. The member opens it in a separate
// browser and chooses a password. Removing the member signs out that browser.
import { openSetup, submitSetup, t, q, fixed } from './harness.mjs';

// The admin's menu under its name, opened; true if it lists `label`.
const menuHas = async (p, label) => {
  await p.eval(`document.querySelector('.name-btn').click()`);
  const has = await p.eval(`[...document.querySelectorAll('.menu-item')].some((e) => e.textContent === ${JSON.stringify(label)})`);
  await p.eval(`document.querySelector('.name-btn').click()`);
  return has;
};

export const area = {
  name: 'members',
  needs: ['auth'],
  async run(h) {
    const a = h.a;
    const member = `e2e-${Date.now().toString(36)}`;

    // ---- the admin creates a member ----
    await a.v(`d.show('list')`);
    h.check('the admin\'s menu has Members', await menuHas(a, t('members.title')));
    await a.eval(`document.querySelector('.name-btn').click()`);
    await a.clickText('.menu-item', t('members.title'), { exact: true });
    await a.until(`document.querySelector('.page[data-page="members"] h1')?.textContent.includes(${q('members.title')})`, 'members page');
    await a.until(`!!document.querySelector('.page[data-page="members"] .btn.primary')`, 'members loaded');
    await a.shot('members', 1440, 900);
    await a.clickText('.page button', t('members.create'), { exact: true });
    await a.until(`document.querySelector('.dialog h2')?.textContent === ${q('members.createTitle')}`, 'create dialog');
    await a.eval(`(() => { const [n, d] = document.querySelectorAll('.dialog input'); n.value = ${JSON.stringify(member)}; d.value = '小成员'; document.querySelector('.dialog button[type="submit"]').click(); })()`);
    await a.until(`document.querySelector('.dialog h2')?.textContent === ${q('members.createdTitle')}`, 'created: invitation dialog');
    // The one "Copy", with this device's clipboard watched instead of written.
    await a.eval(`(() => { window.__copied = []; navigator.clipboard.writeText = async (t) => { window.__copied.push(t); }; })()`);
    await a.clickText('.dialog button', t('members.copy'), { exact: true });
    await a.until(`window.__copied.length === 1`, 'copied');
    const copied = await a.eval(`window.__copied[0]`);
    const link = copied.match(/https?:\/\/\S+/)?.[0];
    h.check('the message includes the invitation and account name', Boolean(link?.includes('/setup#c=') && copied === t('members.message', { link, name: member })));
    await a.clickText('.dialog button', t('common.gotIt'));
    await a.until(`!!document.querySelector('[data-member="${member}"]')`, 'the member is listed');
    h.check('the member is listed with its display name and no computers', await a.eval(`document.querySelector('[data-member="${member}"]').textContent.includes('小成员') && document.querySelector('[data-member="${member}"]').textContent.includes(${q('members.computers', { n: 0 })})`));

    // ---- the member signs in on a device of its own ----
    const m = await h.contextPage();
    await openSetup(m, link);
    h.check('the setup page shows the account and removes the code from the address', await m.eval(`!location.hash && document.querySelector('input[autocomplete=username]').value === ${JSON.stringify(member)}`));
    await m.shot('account-setup', 390, 844);
    const password = 'member own password';
    await submitSetup(m, { password, again: 'not the same' });
    await m.until(`document.querySelector('.setup .error')?.textContent.includes(${q('pw.mismatch')})`, 'mismatch refused');
    await submitSetup(m, { password });
    await m.until(`!!document.querySelector('.empty')`, 'the member\'s empty computer list');
    h.check('after choosing, the member sees an empty computer list', await m.eval(`!document.querySelector('.card')`));
    h.check('a member\'s menu has no Members', !(await menuHas(m, t('members.title'))));
    await m.v(`d.show('members')`);
    h.check('a member cannot open Members', await m.v(`d.view() === 'list'`));

    // ---- the admin removes the member: its page is signed out ----
    await a.v(`d.show('members')`);
    await a.until(`!!document.querySelector('[data-member="${member}"] .more')`, 'the member\'s ⋯');
    await a.eval(`document.querySelector('[data-member="${member}"] .more').click()`);
    await a.clickText('.menu-item', t('members.remove'), { exact: true });
    await a.until(`document.querySelector('.dialog h2')?.textContent.includes(${JSON.stringify(fixed('members.removeTitle'))})`, 'remove confirmation');
    h.check('the confirmation says what goes', await a.eval(`document.querySelector('.dialog p').textContent === ${q('members.removeText', { n: 0 })}`));
    await a.clickText('.dialog button', t('common.remove'), { exact: true });
    await a.until(`!document.querySelector('[data-member="${member}"]')`, 'the member is gone from the list');
    await m.until(`!!document.querySelector('form.login')`, 'the member\'s page is signed out', 20000);
    h.check('removing a member signs its page out', true);
    await h.dropContext(m);
    await a.v(`d.show('list')`);
  },
};
