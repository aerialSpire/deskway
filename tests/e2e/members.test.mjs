// Members (docs/accounts.md), as the pages show it. The test account is the server's admin
// (made with `deskway-server admin`). The admin opens "Members" from the menu under its name and
// creates a member; the one "Copy" copies the link, the account and the initial password. A page
// of its own (another device) signs in as the member: it is let in at once, must choose its
// password and super password before anything else, then sees an empty computer list. The
// member's menu has no "Members". Removing the member signs its page out. The rules behind each
// step are tested in Go (internal/server/members_test.go).
import { signIn, t, q, fixed } from './harness.mjs';

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
    const { url } = h.opts;
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
    await a.until(`document.querySelector('.dialog h2')?.textContent === ${q('members.createdTitle')}`, 'created: the password dialog');
    await a.shot('member-created', 390, 844);
    // The one "Copy", with this device's clipboard watched instead of written.
    await a.eval(`(() => { window.__copied = []; navigator.clipboard.writeText = async (t) => { window.__copied.push(t); }; })()`);
    await a.clickText('.dialog button', t('members.copy'), { exact: true });
    await a.until(`window.__copied.length === 1`, 'copied');
    const copied = await a.eval(`window.__copied[0]`);
    // The message with the password left out: the copy is it, with a password in the gap.
    const [before, after] = t('members.message', { link: `${url}/`, name: member, password: '\0' }).split('\0');
    const password = copied.startsWith(before) && copied.endsWith(after) ? copied.slice(before.length, copied.length - after.length) : '';
    h.check('the copy is the message: Deskway, the link, the account and the initial password', /^\S+$/.test(password), copied);
    await a.clickText('.dialog button', t('common.gotIt'));
    await a.until(`!!document.querySelector('[data-member="${member}"]')`, 'the member is listed');
    h.check('the member is listed with its display name and no computers', await a.eval(`document.querySelector('[data-member="${member}"]').textContent.includes('小成员') && document.querySelector('[data-member="${member}"]').textContent.includes(${q('members.computers', { n: 0 })})`));

    // ---- the member signs in on a device of its own ----
    const m = await h.contextPage();
    await m.send('Page.navigate', { url });
    await m.until(`!!document.querySelector('form.login')`, 'member login form');
    await signIn(m, { user: member, password });
    await m.until(`document.querySelector('.page[data-page="choose"]')`, 'the member must choose its password');
    h.check('the first browser is let in at once and asked for its password and super password', await m.eval(`document.querySelectorAll('.choose input').length === 4`));
    await m.shot('choose-password', 390, 844);
    const own = 'member own password', ownSuper = 'member own super password';
    const choose = (values) => m.eval(`(() => { const ins = document.querySelectorAll('.choose input'); ${JSON.stringify(values)}.forEach((v, i) => { ins[i].value = v; }); document.querySelector('.choose button[type="submit"]').click(); })()`);
    await choose([own, 'not the same', ownSuper, ownSuper]);
    await m.until(`document.querySelector('.choose .error')?.textContent.includes(${q('pw.mismatch')})`, 'mismatch refused');
    await choose([password, password, ownSuper, ownSuper]);
    await m.until(`document.querySelector('.choose .error')?.textContent.includes(${q('choose.same')})`, 'the initial password refused');
    h.check('the initial password cannot be kept', true);
    await choose([own, own, ownSuper, ownSuper]);
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
