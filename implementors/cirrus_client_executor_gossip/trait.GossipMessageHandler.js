(function() {var implementors = {};
implementors["cirrus_client_executor"] = [{"text":"impl&lt;Block, Client, TransactionPool, Backend, CIDP&gt; <a class=\"trait\" href=\"cirrus_client_executor_gossip/trait.GossipMessageHandler.html\" title=\"trait cirrus_client_executor_gossip::GossipMessageHandler\">GossipMessageHandler</a>&lt;Block&gt; for <a class=\"struct\" href=\"cirrus_client_executor/struct.Executor.html\" title=\"struct cirrus_client_executor::Executor\">Executor</a>&lt;Block, Client, TransactionPool, Backend, CIDP&gt; <span class=\"where fmt-newline\">where<br>&nbsp;&nbsp;&nbsp;&nbsp;Block: BlockT,<br>&nbsp;&nbsp;&nbsp;&nbsp;Client: HeaderBackend&lt;Block&gt; + BlockBackend&lt;Block&gt; + ProvideRuntimeApi&lt;Block&gt; + AuxStore + <a class=\"trait\" href=\"https://doc.rust-lang.org/nightly/core/marker/trait.Send.html\" title=\"trait core::marker::Send\">Send</a> + <a class=\"trait\" href=\"https://doc.rust-lang.org/nightly/core/marker/trait.Sync.html\" title=\"trait core::marker::Sync\">Sync</a> + 'static,<br>&nbsp;&nbsp;&nbsp;&nbsp;Client::Api: <a class=\"trait\" href=\"cirrus_primitives/trait.SecondaryApi.html\" title=\"trait cirrus_primitives::SecondaryApi\">SecondaryApi</a>&lt;Block, <a class=\"type\" href=\"cirrus_primitives/type.AccountId.html\" title=\"type cirrus_primitives::AccountId\">AccountId</a>&gt; + BlockBuilder&lt;Block&gt; + ApiExt&lt;Block, StateBackend = StateBackendFor&lt;Backend, Block&gt;&gt;,<br>&nbsp;&nbsp;&nbsp;&nbsp;for&lt;'b&gt; <a class=\"primitive\" href=\"https://doc.rust-lang.org/nightly/std/primitive.reference.html\">&amp;'b </a>Client: BlockImport&lt;Block, Transaction = TransactionFor&lt;Client, Block&gt;, Error = Error&gt;,<br>&nbsp;&nbsp;&nbsp;&nbsp;Backend: Backend&lt;Block&gt; + <a class=\"trait\" href=\"https://doc.rust-lang.org/nightly/core/marker/trait.Send.html\" title=\"trait core::marker::Send\">Send</a> + <a class=\"trait\" href=\"https://doc.rust-lang.org/nightly/core/marker/trait.Sync.html\" title=\"trait core::marker::Sync\">Sync</a> + 'static,<br>&nbsp;&nbsp;&nbsp;&nbsp;TransactionPool: TransactionPool&lt;Block = Block&gt;,<br>&nbsp;&nbsp;&nbsp;&nbsp;CIDP: CreateInherentDataProviders&lt;Block, <a class=\"type\" href=\"cirrus_primitives/type.Hash.html\" title=\"type cirrus_primitives::Hash\">Hash</a>&gt;,&nbsp;</span>","synthetic":false,"types":["cirrus_client_executor::Executor"]}];
if (window.register_implementors) {window.register_implementors(implementors);} else {window.pending_implementors = implementors;}})()