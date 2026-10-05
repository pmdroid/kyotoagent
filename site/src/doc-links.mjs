export default {
  name: 'doc-links',
  link(node, context) {
    if (!/^(?:\.\/)?[\w-]+\.md(?:#.*)?$/.test(node.url)) return;
    const url = node.url.replace(/^(?:\.\/)?([\w-]+)\.md/, (_, name) =>
      name === 'README' ? '/docs/' : `/docs/${name}/`
    );
    context.setProperty(node, 'url', url);
  },
};
